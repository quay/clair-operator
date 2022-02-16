package v1alpha1

import (
	"context"
	"errors"
	"fmt"
	"net/http"

	"github.com/quay/clair/config"
	"gopkg.in/yaml.v3"
	logf "sigs.k8s.io/controller-runtime/pkg/log"
	"sigs.k8s.io/controller-runtime/pkg/webhook/admission"
)

var (
	errMissingLabel = errors.New("missing required label (how?)")
	errBadKind      = errors.New("request object is neither Secret nor ConfigMap")
)

// +kubebuilder:webhook:path=/validate-clair-config,mutating=false,sideEffects=none,failurePolicy=fail,groups="",resources=configmaps;secrets,verbs=create;update,versions=v1,name=vconfig.c.pq.io,admissionReviewVersions=v1;v1beta1

// ConfigValidator is a validating webhook that disallows updates or creations
// of labelled ConfigMaps or Secrets with malformed Clair configurations.
//
// To opt into this behavior, a ConfigMap or Secret must have the label
// [ConfigLabel] ("clair.projectquay.io/config"), with the value being the
// version of the Clair config. This is currently only "v1".
//
// The annotation [ConfigKey] ("clair.projectquay.io/config-key") must be
// present. The value is used as the key containing the configuration to
// validate. If using the ConfigMutator, this key will be autopopulated if not
// present.
//
// +kubebuilder:object:generate=false
type ConfigValidator struct {
	configCommon
}

// Handle implements admission.Handler.
func (v *ConfigValidator) Handle(ctx context.Context, req admission.Request) admission.Response {
	log := logf.FromContext(ctx).
		WithName("validator").
		WithValues("uid", req.UID)
	ctx = logf.IntoContext(ctx, log)
	log.Info("examining object",
		"namespace", req.Namespace,
		"name", req.Name,
		"kind", req.Kind)

	// Populate the labels, annotations, and data.
	// Errors reported here are errors in the request.
	d, err := v.details(ctx, req)
	if err != nil {
		log.Info("NO", "reason", "bad request", "error", err.Error())
		return admission.Errored(http.StatusBadRequest, err)
	}
	// Grab the config version from the label. If the label doesn't exist, how
	// did this validator even get called?
	version, ok := d.labels[ConfigLabel]
	if !ok {
		log.Info("NO", "reason", "martian request")
		return admission.Errored(http.StatusBadRequest, errMissingLabel)
	}
	log.V(1).Info("labelled as", "version", version)

	// Find the key containing the configuration blob.
	key, ok := d.annotations[ConfigKey]
	if !ok {
		log.Info("NO", "reason", "missing annotation")
		return admission.Denied("missing required annotation: indicate key containing config")
	}
	log.V(1).Info("config at", "key", key)

	// Grab the blob.
	cfg, ok := d.item(key)
	if !ok {
		log.Info("NO", "reason", "missing config")
		return admission.Denied(fmt.Sprintf("missing value: indicated key %q does not exist", key))
	}

	if err := validateConfig(ctx, version, cfg); err != nil {
		log.Info("NO", "reason", "validation failed", "error", err.Error())
		return admission.Denied(fmt.Sprintf("config validation failed: %v", err))
	}
	log.Info("OK")
	return admission.Allowed("")
}

// ValidateConfig is the workhorse function that takes raw bytes and is
// responsible for checking correctness. A nil error is reported if the config
// is valid.
//
// A version string is passed for forwards compatibility.
func validateConfig(ctx context.Context, v string, b []byte) error {
	if err := ctx.Err(); err != nil {
		return err
	}
	log := logf.FromContext(ctx)

	switch v {
	case ConfigLabelV1:
		var c config.Config
		if err := yaml.Unmarshal(b, &c); err != nil {
			return err
		}
		var err error
		for _, m := range []string{"indexer", "matcher", "notifier"} {
			c.Mode, err = config.ParseMode(m)
			if err != nil {
				return err
			}
			ws, err := config.Validate(&c)
			if err != nil {
				return err
			}
			for _, w := range ws {
				log.V(1).Info("lint", "msg", w.Error())
			}
			log.V(1).Info("validated", "mode", m)
		}
	default:
		return fmt.Errorf("unknown config version: %q", v)
	}

	// Additional Validation? Lints here?
	return nil
}
