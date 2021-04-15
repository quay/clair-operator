package v1alpha1

import (
	"bytes"
	"context"
	"encoding/base64"
	"errors"
	"fmt"
	"net"
	"net/http"
	"net/url"
	"path"
	"strconv"
	"strings"

	"github.com/go-logr/logr"
	"gomodules.xyz/jsonpatch/v2"
	"gopkg.in/yaml.v3"
	corev1 "k8s.io/api/core/v1"
	"sigs.k8s.io/controller-runtime/pkg/webhook/admission"
)

// +kubebuilder:webhook:path=/mutate-clair-config,mutating=true,sideEffects=none,failurePolicy=fail,groups="",resources=configmaps;secrets,verbs=create;update,versions=v1,name=mconfig.c.pq.io,admissionReviewVersions=v1;v1beta1

// ConfigMutator ...
//
// +kubebuilder:object:generate=false
type ConfigMutator struct {
	configCommon
}

// Handle implements admission.Handler.
func (m *ConfigMutator) Handle(ctx context.Context, req admission.Request) admission.Response {
	log := configlog.
		WithName("mutator").
		WithValues("uid", req.UID)
	ctx = logr.NewContext(ctx, log)
	log.Info("examining object",
		"namespace", req.Namespace,
		"name", req.Name,
		"kind", req.Kind)

	d, err := m.details(ctx, req)
	if err != nil {
		return admission.Errored(http.StatusBadRequest, err)
	}
	ops := []jsonpatch.Operation{
		jsonpatch.Operation{Path: `/data/`, Operation: `add`},
	}
	annot := map[string]string{}
	var warn []string

	version, ok := d.labels[ConfigLabel]
	if !ok {
		return admission.Errored(http.StatusBadRequest, errMissingLabel)
	}
	log.V(1).Info("labelled as", "version", version)

	inKey, ok := d.annotations[TemplateKey]
	if !ok {
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
			Value:     outKey})
		log.V(1).Info("creating an output file")
		annot[`output-guessed`] = outKey
	}
	ops[0].Path += outKey
	in, ok := d.item(inKey)
	if !ok {
		return admission.Denied(fmt.Sprintf("key does not exist: %s", inKey))
	}

	log.V(1).Info("attempting templating", "input_key", inKey, "output_key", outKey)
	t, err := m.template(ctx, version, &d, in)
	if err != nil {
		return admission.Errored(http.StatusPreconditionFailed, err)
	}
	log.V(3).Info("templated", "out", t.String())
	switch req.Kind.Kind {
	case "Secret":
		ops[0].Value = base64.StdEncoding.EncodeToString(t.Bytes())
	case "ConfigMap":
		ops[0].Value = t.String()
	default:
		panic("unreachable")
	}

	res := admission.Patched("template ok", ops...)
	res.Warnings = append(warn, t.ws...)
	res.AuditAnnotations = annot
	return res
}

// Template does the templating.
func (m *ConfigMutator) template(ctx context.Context, v string, d *configDetails, in []byte) (*tmpl, error) {
	// TODO(hank) This should return warnings that can be propagated to the
	// response.
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
		return nil, fmt.Errorf("unknown config version: %q", v)
	}

	return &out, nil
}

// Tmpl is output and a list of warnings.
//
// This can be used to flag migration issues or differences in generation. Not
// fully plumbed through, though.
type tmpl struct {
	bytes.Buffer
	ws []string
}

func (t *tmpl) warn(msg string) {
	t.ws = append(t.ws, msg)
}

// TemplateV1 does the templating for V1 configs.
func (m *ConfigMutator) templateV1(ctx context.Context, out *tmpl, in []byte, d *configDetails) error {
	if err := ctx.Err(); err != nil {
		return err
	}
	log := logr.FromContext(ctx)

	var n yaml.Node
	if err := yaml.Unmarshal(in, &n); err != nil {
		return err
	}

	// Look for references and dereference them.
	type resolveFunc func(string) (string, error)
	var examine func(n *yaml.Node) error
	resolve := func(in string) (string, error) { return m.resolveURIs(ctx, d, in) }

	examine = func(n *yaml.Node) error {
		if n.Kind == yaml.ScalarNode && n.ShortTag() == `!!str` {
			out, err := resolve(n.Value)
			if err != nil {
				log.Info("errored templating string", "error", err, "in", n.Value)
				return err
			}
			if out != n.Value {
				log.V(2).Info("setting string",
					"in", in, "out", out)
				n.Value = out
			}
			return nil
		}
		for _, n := range n.Content {
			if err := examine(n); err != nil {
				return err
			}
		}
		return nil
	}
	if err := examine(&n); err != nil {
		return err
	}

	enc := yaml.NewEncoder(out)
	enc.SetIndent(2)
	if err := enc.Encode(&n); err != nil {
		return err
	}
	return nil
}

// ResolveURIs looks for our special URIs and then attempts to resolve them in
// the current context.
//
// TODO(hank) This all needs to be documented.
func (m *ConfigMutator) resolveURIs(ctx context.Context, d *configDetails, in string) (string, error) {
	const (
		asDB = 1 << iota
		asPg
	)
	var flags uint64
	log := logr.FromContext(ctx)

	u, err := url.Parse(in)
	if err != nil {
		log.V(3).Error(err, "not a URL")
		return in, nil
	}

	resolveFromKeys := func(d *configDetails, v url.Values) (string, bool) {
		var out string
		switch {
		case flags&asDB != 0:
			switch {
			case flags&asPg != 0:
				out = resolvePostgres(d.strData, d.data).String()
			default:
				panic("programmer error")
			}
		default:
			ks, ok := v["key"]
			if !ok {
				return "", false
			}
			vs := make([][]byte, 0, len(ks))
			for _, k := range ks {
				if x, ok := d.item(k); ok {
					vs = append(vs, x)
				}
			}
			out = string(bytes.Join(vs, []byte(v.Get("join"))))
		}
		return out, true
	}

	var out string
Scheme:
	switch u.Scheme {
	case `secret`:
		if !d.isSecret {
			return in, errors.New(`cannot reference secret from config in non-secret`)
		}
		if u.Opaque == "" {
			log.Info(`found malformed service URI, skipping`, "url", u.String())
			break
		}

		var sec corev1.Secret
		if err := m.client.Get(ctx, toName(u.Opaque), &sec); err != nil {
			return in, err
		}
		var rd configDetails
		if err := rd.fromSecret(&sec); err != nil {
			return in, err
		}
		res, ok := resolveFromKeys(&rd, u.Query())
		if !ok {
			log.Info(`URI missing "key" parameter`, "url", u.String())
			return in, nil
		}

		out = res
	case `configmap`:
		if u.Opaque == "" {
			log.Info(`found malformed service URI, skipping`, "url", u.String())
			break
		}

		var cm corev1.ConfigMap
		if err := m.client.Get(ctx, toName(u.Opaque), &cm); err != nil {
			return in, err
		}
		var rd configDetails
		if err := rd.fromConfigMap(&cm); err != nil {
			return in, err
		}
		res, ok := resolveFromKeys(&rd, u.Query())
		if !ok {
			log.Info(`URI missing "key" parameter`, "url", u.String())
			return in, nil
		}

		out = res
	case `database+postgres`:
		if u.Opaque == "" {
			log.Info(`found malformed database URI, skipping`, "url", u.String())
			break
		}

		su, err := url.Parse(u.Opaque)
		if err != nil {
			log.Info(`found malformed database URI, skipping`,
				"url", u.String(),
				"err", err.Error(),
			)
			break
		}
		u = su
		flags |= asDB | asPg
		goto Scheme
	case `service`:
		if u.Opaque == "" {
			log.Info(`found malformed service URI, skipping`, "url", u.String())
			break
		}

		v := u.Query()
		name := PortAPI
		if n, ok := v[`portname`]; ok {
			name = n[0]
		}
		var srv corev1.Service
		if err := m.client.Get(ctx, toName(u.Opaque), &srv); err != nil {
			return in, err
		}
		var port *corev1.ServicePort
		for i, p := range srv.Spec.Ports {
			if p.Name == name {
				port = &srv.Spec.Ports[i]
				break
			}
		}
		if port == nil {
			log.Info(`unable to find expected port name`,
				"name", name,
				"service", srv.String(),
			)
			return in, nil
		}
		u := url.URL{
			Scheme: `http`,
			Host:   fmt.Sprintf("%s.%s.srv", srv.Name, srv.Namespace),
		}
		if s, ok := v[`scheme`]; ok {
			u.Scheme = s[0]
		}
		switch u.Scheme {
		case `http`:
			if port.Port != 80 {
				u.Host = net.JoinHostPort(u.Host, strconv.Itoa(int(port.Port)))
			}
		case `https`:
			if port.Port != 443 {
				u.Host = net.JoinHostPort(u.Host, strconv.Itoa(int(port.Port)))
			}
		}
		log.V(2).Info("resolved service", "uri", u.String(), "name", name)
		out = u.String()
	case `indexer`, `matcher`, `notifier`:
		var key string
		switch u.Scheme {
		case `indexer`:
			key = TemplateIndexerService
		case `matcher`:
			key = TemplateMatcherService
		case `notifier`:
			key = TemplateNotifierService
		}
		n, ok := d.annotations[key]
		if !ok {
			log.Info(`scheme used, but annotation not present`,
				"scheme", u.Scheme,
				"annotation", key,
			)
			break
		}
		su, err := url.Parse(`service:` + n)
		if err != nil {
			panic("programmer error: couldn't construct service URI")
		}
		su.RawQuery = u.RawQuery
		u = su
		goto Scheme
	default:
		log.V(2).Info(`ignoring unsupported scheme`, "scheme", u.Scheme)
		out = in
	}

	return out, nil
}

// ResolvePostgres looks at the keys in the provided maps and interprets them as
// a bunch of libpq environment variables.
func resolvePostgres(d map[string]string, b map[string][]byte) *url.URL {
	out := struct {
		Host, Port, Database, User, Password string
	}{}
	vs := url.Values{}
	for k, q := range pgSecrets {
		x := d[k]
		if x == "" {
			v, ok := b[k]
			if !ok {
				continue
			}
			x = string(v)
		}
		switch k {
		case "PGHOST":
			out.Host = x
		case "PGPORT":
			out.Port = x
		case "PGDATABASE":
			out.Database = x
		case "PGUSER":
			out.User = x
		case "PGPASSWORD":
			out.Password = x
		default:
			vs.Set(q, x)
		}
	}
	ou := url.URL{
		Scheme:   `postgresql`,
		Host:     net.JoinHostPort(out.Host, out.Port),
		User:     url.UserPassword(out.User, out.Password),
		Path:     "/" + out.Database,
		RawQuery: vs.Encode(),
	}
	return &ou
}

var pgSecrets = map[string]string{
	"PGHOST":               "",
	"PGPORT":               "",
	"PGDATABASE":           "",
	"PGUSER":               "",
	"PGPASSWORD":           "",
	"PGSSLMODE":            "sslmode",
	"PGSSLCERT":            "sslcert",
	"PGSSLKEY":             "sslkey",
	"PGSSLROOTCERT":        "sslrootcert",
	"PGAPPNAME":            "application_name",
	"PGCONNECT_TIMEOUT":    "connect_timeout",
	"PGTARGETSESSIONATTRS": "target_session_attrs",
}
