/*
Copyright 2021 The Clair authors.

Licensed under the Apache License, Version 2.0 (the "License");
you may not use this file except in compliance with the License.
You may obtain a copy of the License at

    http://www.apache.org/licenses/LICENSE-2.0

Unless required by applicable law or agreed to in writing, software
distributed under the License is distributed on an "AS IS" BASIS,
WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
See the License for the specific language governing permissions and
limitations under the License.
*/

package v1alpha1

import (
	"bytes"
	"context"
	"errors"
	"fmt"
	"net"
	"net/http"
	"net/url"
	"strconv"
	"strings"

	"github.com/go-logr/logr"
	"github.com/quay/clair/v4/config"
	"gomodules.xyz/jsonpatch/v2"
	"gopkg.in/yaml.v3"
	appsv1 "k8s.io/api/apps/v1"
	corev1 "k8s.io/api/core/v1"
	"k8s.io/apimachinery/pkg/types"
	ctrl "sigs.k8s.io/controller-runtime"
	"sigs.k8s.io/controller-runtime/pkg/client"
	logf "sigs.k8s.io/controller-runtime/pkg/log"
	"sigs.k8s.io/controller-runtime/pkg/webhook"
	"sigs.k8s.io/controller-runtime/pkg/webhook/admission"
)

// Configlog is for logging in the config validator.
var configlog = logf.Log.WithName("clair-config-validator")

func SetupConfigWebhooks(mgr ctrl.Manager) error {
	hookServer := mgr.GetWebhookServer()
	configlog.Info("registering webhooks to webhook server")
	v, m := &ConfigValidator{}, &ConfigMutator{}
	v.client = mgr.GetClient()
	m.client = mgr.GetClient()
	hookServer.Register("/validate-clair-config", &webhook.Admission{Handler: v})
	hookServer.Register("/mutate-clair-config", &webhook.Admission{Handler: m})
	return nil
}

// +kubebuilder:object:generate=false
type ConfigMutator struct {
	configCommon
}

func (m *ConfigMutator) Handle(ctx context.Context, req admission.Request) admission.Response {
	log := configlog.WithValues("uid", req.UID)
	log.V(1).Info("examining object",
		"namespace", req.Namespace,
		"name", req.Name,
		"kind", req.Kind)

	d, err := m.details(ctx, req)
	if err != nil {
		return admission.Errored(http.StatusBadRequest, err)
	}
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
		return admission.Allowed("config key not provided")
	}
	switch req.Kind.Kind {
	case "Secret":
		outKey = `binaryData/` + outKey
	case "ConfigMap":
		outKey = `data/` + outKey
	default:
		panic("unreachable")
	}
	in, ok := d.data[inKey]
	if !ok {
		return admission.Denied(fmt.Sprintf("key does not exist: %s", inKey))
	}
	out, err := m.template(ctx, log, d.annotations, version, in)
	if err != nil {
		return admission.Errored(http.StatusPreconditionFailed, err)
	}

	return admission.Patched("template ok", jsonpatch.Operation{
		Path:      outKey,
		Operation: `replace`,
		Value:     out,
	})
}

func (m *ConfigMutator) template(ctx context.Context, log logr.Logger, a map[string]string, v string, in []byte) ([]byte, error) {
	if err := ctx.Err(); err != nil {
		return nil, err
	}
	var buf bytes.Buffer

	log.Info("templating configuration", "version", v)
	switch v {
	case ConfigLabelV1:
		portCt := map[int32]struct{}{}
		var c config.Config
		if err := yaml.Unmarshal(in, &c); err != nil {
			return nil, err
		}
		for key, name := range a {
			switch key {
			case TemplateIndexerService, TemplateMatcherService, TemplateNotifierService:
				log.V(1).Info("examining service", "annotation", key, "name", name)
				var srv corev1.Service
				if err := m.client.Get(ctx, toName(name), &srv); err != nil {
					return nil, err
				}
				var port *corev1.ServicePort
				for i, p := range srv.Spec.Ports {
					if p.Name == PortAPI {
						port = &srv.Spec.Ports[i]
						break
					}
				}
				if port == nil {
					return nil, fmt.Errorf("missing expected port %q", PortAPI)
				}
				u := url.URL{
					Scheme: `http`,
					Host:   fmt.Sprintf("%s.%s.srv", srv.Name, srv.Namespace),
				}
				if port.Port != 80 {
					u.Host = net.JoinHostPort(u.Host, strconv.Itoa(int(port.Port)))
				}
				log.V(1).Info("resolved service", "uri", u.String(), "name", name)

				switch key {
				case TemplateIndexerService:
					if c.Matcher.IndexerAddr == `indexer://` {
						log.V(1).Info(`replacing matcher's "IndexerAddr"`, "uri", u.String(), "name", name)
						c.Matcher.IndexerAddr = u.String()
					}
					if c.Notifier.IndexerAddr == `indexer://` {
						log.V(1).Info(`replacing notifier's "IndexerAddr"`, "uri", u.String(), "name", name)
						c.Notifier.IndexerAddr = u.String()
					}
				case TemplateMatcherService:
					if c.Notifier.MatcherAddr == `matcher://` {
						log.V(1).Info(`replacing notifier's "MatcherAddr"`, "uri", u.String(), "name", name)
						c.Notifier.MatcherAddr = u.String()
					}
				case TemplateNotifierService:
					// Nothing in the system makes requests to the notifier.
				}
			case TemplateIndexerDeployment, TemplateMatcherDeployment, TemplateNotifierDeployment:
				var d appsv1.Deployment
				log.V(1).Info("examining deployment", "annotation", key, "name", name)
				if err := m.client.Get(ctx, toName(name), &d); err != nil {
					return nil, err
				}
				var cr *corev1.Container
				for i := range d.Spec.Template.Spec.Containers {
					sc := &d.Spec.Template.Spec.Containers[i]
					if sc.Name == `clair` {
						cr = sc
						break
					}
				}
				if cr == nil {
					return nil, fmt.Errorf("missing expected container %q", `clair`)
				}
				for _, p := range cr.Ports {
					switch p.Name {
					case PortAPI:
						log.V(1).Info(`replacing config's "HTTPListenAddr"`, "port", p.ContainerPort)
						portCt[p.ContainerPort] = struct{}{}
						c.HTTPListenAddr = fmt.Sprintf(":%d", p.ContainerPort)
					case PortIntrospection:
						log.V(1).Info(`replacing config's "IntrospectionAddr"`, "port", p.ContainerPort)
						portCt[p.ContainerPort] = struct{}{}
						c.IntrospectionAddr = fmt.Sprintf(":%d", p.ContainerPort)
					}
				}
			}
		}

		// Look for secrets and dereference them.
		if u, err := url.Parse(c.Indexer.ConnString); err == nil && u.Scheme == `secret` {
			log.V(1).Info(`found secret reference`, "key", "/indexer/conn_string")
			var s corev1.Secret
			if err := m.client.Get(ctx, toName(u.Opaque), &s); err != nil {
				return nil, err
			}
			u, err := resolveDatabaseSecret(&s)
			if err != nil {
				return nil, err
			}
			log.V(1).Info(`replacing secret reference`, "key", "/indexer/conn_string")
			c.Indexer.ConnString = u.String()
		}
		if u, err := url.Parse(c.Matcher.ConnString); err == nil && u.Scheme == `secret` {
			log.V(1).Info(`found secret reference`, "key", "/matcher/conn_string")
			var s corev1.Secret
			if err := m.client.Get(ctx, toName(u.Opaque), &s); err != nil {
				return nil, err
			}
			u, err := resolveDatabaseSecret(&s)
			if err != nil {
				return nil, err
			}
			log.V(1).Info(`replacing secret reference`, "key", "/matcher/conn_string")
			c.Matcher.ConnString = u.String()
		}
		if u, err := url.Parse(c.Notifier.ConnString); err == nil && u.Scheme == `secret` {
			log.V(1).Info(`found secret reference`, "key", "/notifier/conn_string")
			var s corev1.Secret
			if err := m.client.Get(ctx, toName(u.Opaque), &s); err != nil {
				return nil, err
			}
			u, err := resolveDatabaseSecret(&s)
			if err != nil {
				return nil, err
			}
			log.V(1).Info(`replacing secret reference`, "key", "/notifier/conn_string")
			c.Notifier.ConnString = u.String()
		}

		if len(portCt) > 2 {
			ps := make([]int32, 0, len(portCt))
			for p := range portCt {
				ps = append(ps, p)
			}
			// The deployments are configured for multiple different ports,
			// which shouldn't have happened.
			//
			// Some rectify loop could fix this, but I don't think this is the
			// place.
			return nil, fmt.Errorf("deployments are configured for multiple different ports: %v", ps)
		}
		if err := yaml.NewEncoder(&buf).Encode(&c); err != nil {
			return nil, err
		}
	default:
		return nil, fmt.Errorf("unknown config version: %q", v)
	}

	return buf.Bytes(), nil
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

func resolveDatabaseSecret(s *corev1.Secret) (*url.URL, error) {
	out := struct {
		Host, Port, Database, User, Password string
	}{}
	vs := url.Values{}
	for k, q := range pgSecrets {
		x := s.StringData[k]
		if v := string(s.Data[k]); x == "" && v != "" {
			x = v
		}
		if x == "" {
			continue
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
	return &ou, nil
}

func toName(s string) types.NamespacedName {
	i := strings.IndexByte(s, '/')
	return types.NamespacedName{
		Namespace: s[:i],
		Name:      s[i+1:],
	}
}

// +kubebuilder:webhook:path=/validate-clair-config,mutating=false,sideEffects=none,failurePolicy=fail,groups="",resources=configmaps;secrets,verbs=create;update,versions=v1,name=vconfig.c.pq.io,admissionReviewVersions=v1;v1beta1

// ConfigValidator is a validating webhook that disallows updates or creations
// of labelled ConfigMaps or Secrets with malformed Clair configurations.
//
// +kubebuilder:object:generate=false
type ConfigValidator struct {
	configCommon
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

	switch v {
	case ConfigLabelV1:
		var c config.Config
		if err := yaml.Unmarshal(b, &c); err != nil {
			return err
		}
		for _, m := range []string{"indexer", "matcher", "notifier"} {
			c.Mode = m
			if err := config.Validate(c); err != nil {
				return err
			}
		}
	default:
		return fmt.Errorf("unknown config version: %q", v)
	}

	// Additional Validation?
	return nil
}

const (
	// ConfigLabel is label needed to trigger the validating webhook.
	ConfigLabel = `clair.projectquay.io/config`
	// ConfigAnnotation is the annotation used to indicate which key contains
	// the config blob.
	ConfigAnnotation = `clair.projectquay.io/config`

	// ConfigLabelV1 and friends indicate the valid values for the ConfigLabel.
	ConfigLabelV1 = `v1`
)

var (
	errMissingLabel = errors.New("missing required label (how?)")
	errBadKind      = errors.New("request object is neither Secret nor ConfigMap")
)

func (v *ConfigValidator) Handle(ctx context.Context, req admission.Request) admission.Response {
	log := configlog.WithValues("uid", req.UID)
	log.V(1).Info("examining object",
		"namespace", req.Namespace,
		"name", req.Name,
		"kind", req.Kind)

	// Populate the labels, annotations, and data.
	// Errors reported here are errors in the request.
	d, err := v.details(ctx, req)
	if err != nil {
		return admission.Errored(http.StatusBadRequest, err)
	}
	// Grab the config version from the label. If the label doesn't exist, how
	// did this validator even get called?
	version, ok := d.labels[ConfigLabel]
	if !ok {
		return admission.Errored(http.StatusBadRequest, errMissingLabel)
	}
	log.V(1).Info("labelled as", "version", version)

	// Find the key containing the configuration blob.
	key, ok := d.annotations[ConfigAnnotation]
	if !ok {
		return admission.Denied("missing required annotation: indicate key containing config")
	}
	log.V(1).Info("config at", "key", key)

	// Grab the blob.
	cfg, ok := d.data[key]
	if !ok {
		return admission.Denied(fmt.Sprintf("missing value: indicated key %q does not exist", key))
	}

	if err := validateConfig(ctx, version, cfg); err != nil {
		return admission.Denied(fmt.Sprintf("config validation failed: %v", err))
	}
	return admission.Allowed("")
}

type configDetails struct {
	labels      map[string]string
	annotations map[string]string
	data        map[string][]byte
}

type configCommon struct {
	client  client.Client
	decoder *admission.Decoder
}

// InjectClient implements inject.Client.
func (c *configCommon) InjectClient(cl client.Client) error {
	c.client = cl
	return nil
}

// InjectDecoder implements admission.DecoderInjector.
func (c *configCommon) InjectDecoder(d *admission.Decoder) error {
	c.decoder = d
	return nil
}

func (c *configCommon) details(ctx context.Context, req admission.Request) (configDetails, error) {
	var cfg configDetails
	// Populate the labels, annotations, and data.
	// Errors reported here are errors in the request.
	switch req.Kind.Kind {
	case "Secret":
		secret := corev1.Secret{}
		if err := c.decoder.Decode(req, &secret); err != nil {
			return cfg, fmt.Errorf("unable to decode request: %v", err)
		}
		cfg.labels = secret.Labels
		cfg.annotations = secret.Annotations
		cfg.data = secret.Data
	case "ConfigMap":
		config := corev1.ConfigMap{}
		if err := c.decoder.Decode(req, &config); err != nil {
			return cfg, fmt.Errorf("unable to decode request: %v", err)
		}
		cfg.labels = config.Labels
		cfg.annotations = config.Annotations
		cfg.data = make(map[string][]byte)
		for k, b := range config.BinaryData {
			cfg.data[k] = b
		}
		for k, s := range config.Data {
			cfg.data[k] = []byte(s)
		}
	default:
		return cfg, errBadKind
	}
	return cfg, nil
}
