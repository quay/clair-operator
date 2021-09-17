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
	"context"
	"fmt"
	"net/http"
	"strings"

	corev1 "k8s.io/api/core/v1"
	"k8s.io/apimachinery/pkg/types"
	ctrl "sigs.k8s.io/controller-runtime"
	"sigs.k8s.io/controller-runtime/pkg/client"
	logf "sigs.k8s.io/controller-runtime/pkg/log"
	"sigs.k8s.io/controller-runtime/pkg/webhook"
	"sigs.k8s.io/controller-runtime/pkg/webhook/admission"
)

func SetupConfigWebhooks(mgr ctrl.Manager) error {
	log := mgr.GetLogger().WithName("clair-config")
	hookServer := mgr.GetWebhookServer()
	log.Info("registering webhooks")
	injectLogger := func(ctx context.Context, _ *http.Request) context.Context {
		return logf.IntoContext(ctx, log)
	}
	_ = injectLogger
	hookServer.Register("/validate-clair-config", &webhook.Admission{
		Handler:         &ConfigValidator{},
		WithContextFunc: injectLogger,
	})
	hookServer.Register("/mutate-clair-config", &webhook.Admission{
		Handler:         &ConfigMutator{},
		WithContextFunc: injectLogger,
	})
	return nil
}

// OpPath is a Replacer for escaping the paths in jsonpatch operation paths.
var opPath = strings.NewReplacer("~", "~0", "/", "~1")

func toName(s string) types.NamespacedName {
	i := strings.IndexByte(s, '/')
	t := types.NamespacedName{
		Name: s[i+1:],
	}
	if i != -1 {
		t.Namespace = s[:i]
	}
	return t
}

// ConfigDetails normalizes a ConfigMap or Secret into the common elements.
type configDetails struct {
	labels      map[string]string
	annotations map[string]string
	data        map[string][]byte
	strData     map[string]string
	isSecret    bool
}

func (d *configDetails) item(k string) (v []byte, ok bool) {
	v, ok = d.data[k]
	if ok {
		return v, true
	}
	s, ok := d.strData[k]
	if ok {
		return []byte(s), true
	}
	return nil, false
}

func (d *configDetails) fromSecret(s *corev1.Secret) error {
	d.isSecret = true
	d.labels = s.Labels
	d.annotations = s.Annotations
	d.data = s.Data
	d.strData = s.StringData
	return nil
}

func (d *configDetails) fromConfigMap(c *corev1.ConfigMap) error {
	d.isSecret = false
	d.labels = c.Labels
	d.annotations = c.Annotations
	d.data = c.BinaryData
	d.strData = c.Data
	return nil
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

// Details fetches the concrete type of the Object specified in the Request,
// then normalizes it into a configDetails.
func (c *configCommon) details(ctx context.Context, req admission.Request) (configDetails, error) {
	var cfg configDetails
	// Populate the labels, annotations, and data.
	// Errors reported here are errors in the request.
	switch req.Kind.Kind {
	case "Secret":
		s := corev1.Secret{}
		if err := c.decoder.Decode(req, &s); err != nil {
			return cfg, fmt.Errorf("unable to decode request: %v", err)
		}
		if err := cfg.fromSecret(&s); err != nil {
			return cfg, fmt.Errorf("unable to make sense of request: %v", err)
		}
	case "ConfigMap":
		cm := corev1.ConfigMap{}
		if err := c.decoder.Decode(req, &cm); err != nil {
			return cfg, fmt.Errorf("unable to decode request: %v", err)
		}
		if err := cfg.fromConfigMap(&cm); err != nil {
			return cfg, fmt.Errorf("unable to make sense of request: %v", err)
		}
	default:
		return cfg, errBadKind
	}
	return cfg, nil
}
