package v1alpha1

import (
	"context"
	"encoding/base64"
	"fmt"
	"net/http"
	"path"
	"strings"

	"github.com/go-logr/logr"
	"gomodules.xyz/jsonpatch/v2"
	logf "sigs.k8s.io/controller-runtime/pkg/log"
	"sigs.k8s.io/controller-runtime/pkg/webhook/admission"
)

// +kubebuilder:webhook:path=/mutate-clair-config,mutating=true,sideEffects=none,failurePolicy=fail,groups="",resources=configmaps;secrets,verbs=create;update,versions=v1,name=mconfig.c.pq.io,admissionReviewVersions=v1;v1beta1

// ConfigMutator implements a mutating webhook that takes an input and runs it
// through a templating engine to produce a new config to be used by Clair. See
// the package documentation for the details of the templating language.
//
// To opt into this behavior, a ConfigMap or Secret must have the label
// [ConfigLabel] ("clair.projectquay.io/config"), with the value being the
// version of the Clair config. This is currently only "v1".
//
// Once an Object is being watched, the value of the annotation [TemplateKey]
// ("clair.projectquay.io/config-template-key") is used as a key to read the
// template from. If the annotation [ConfigKey]
// ("clair.projectquay.io/config-key") is populated, it is used as a key to
// write the templated config to. If not provided, a default is guessed;
// [TemplateKey] with the extension removed, or with ".yaml" added if there was
// not an extension.
//
// +kubebuilder:object:generate=false
type ConfigMutator struct {
	configCommon
}

// Handle implements admission.Handler.
func (m *ConfigMutator) Handle(ctx context.Context, req admission.Request) admission.Response {
	log := logf.FromContext(ctx).
		WithName("mutator").
		WithValues("uid", req.UID)
	ctx = logf.IntoContext(ctx, log)
	log.Info("examining object",
		"namespace", req.Namespace,
		"name", req.Name,
		"kind", req.Kind)

	d, err := m.details(ctx, req)
	if err != nil {
		log.Info("NO", "reason", "bad request", "error", err.Error())
		return admission.Errored(http.StatusBadRequest, err)
	}
	ops := []jsonpatch.Operation{
		{Path: `/data/`, Operation: `add`},
	}
	annot := map[string]string{}

	version, ok := d.labels[ConfigLabel]
	if !ok {
		log.Info("NO", "reason", "martian request")
		return admission.Errored(http.StatusBadRequest, errMissingLabel)
	}
	log.V(1).Info("labelled as", "version", version)

	inKey, ok := d.annotations[TemplateKey]
	if !ok {
		log.Info("SKIP", "reason", "missing input annotation")
		return admission.Allowed("template key not provided")
	}
	outKey, ok := d.annotations[ConfigKey]
	if !ok {
		outKey = strings.TrimSuffix(inKey, path.Ext(inKey))
		if outKey == inKey { // If it didn't have an extension suffix
			outKey += ".yaml"
		}
		ops = append(ops, jsonpatch.Operation{
			Path:      `/metadata/annotations/` + opPath.Replace(ConfigKey),
			Operation: `add`,
			Value:     outKey,
		})
		log.Info("output key not specified, generated key", "key", outKey)
		annot[`output-guessed`] = outKey
	}
	ops[0].Path += outKey
	in, ok := d.item(inKey)
	if !ok {
		log.Info("NO", "reason", "input key missing", "key", inKey)
		return admission.Denied(fmt.Sprintf("key does not exist: %s", inKey))
	}

	log.V(1).Info("attempting templating", "input_key", inKey, "output_key", outKey)
	t, err := m.template(ctx, version, &d, in)
	if err != nil {
		return admission.Errored(http.StatusPreconditionFailed, err)
	}
	switch req.Kind.Kind {
	case "Secret":
		ops[0].Value = base64.StdEncoding.EncodeToString(t.Bytes())
	case "ConfigMap":
		ops[0].Value = t.String()
	default:
		panic("unreachable")
	}

	res := admission.Patched("template ok", ops...)
	res.Warnings = append(res.Warnings, t.ws...)
	if w := res.Warnings; len(w) != 0 {
		log.V(1).Info("returned warnings", "warnings", w)
	}
	res.AuditAnnotations = annot
	log.Info("OK")
	return res
}

// Template does the templating.
func (m *ConfigMutator) template(ctx context.Context, v string, d *configDetails, in []byte) (*tmpl, error) {
	if err := ctx.Err(); err != nil {
		return nil, err
	}
	log := logr.FromContextOrDiscard(ctx)
	var out tmpl

	log.Info("templating configuration", "version", v)
	switch v {
	case ConfigLabelV1:
		if err := m.templateV1(ctx, &out, in, d); err != nil {
			return nil, err
		}
	default:
		return nil, fmt.Errorf("unknown config version: %#q", v)
	}

	return &out, nil
}
