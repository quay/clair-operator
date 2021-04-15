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
	"errors"

	appsv1 "k8s.io/api/apps/v1"
	scalev2 "k8s.io/api/autoscaling/v2beta2"
	corev1 "k8s.io/api/core/v1"
	metav1 "k8s.io/apimachinery/pkg/apis/meta/v1"
)

// Labels ...
const (
	// ConfigLabel is label needed to trigger the validating webhook.
	ConfigLabel = `clair.projectquay.io/config`

	// ConfigLabelV1 and friends indicate the valid values for the ConfigLabel.
	ConfigLabelV1 = `v1`
)

// Annotations ...
const (
	// ConfigKey is the annotation used to indicate which key contains
	// the config blob.
	ConfigKey   = `clair.projectquay.io/config-key`
	TemplateKey = `clair.projectquay.io/config-template-key`

	TemplateIndexerService     = `clair.projectquay.io/template-indexer-service`
	TemplateIndexerDeployment  = `clair.projectquay.io/template-indexer-deployment`
	TemplateMatcherService     = `clair.projectquay.io/template-matcher-service`
	TemplateMatcherDeployment  = `clair.projectquay.io/template-matcher-deployment`
	TemplateNotifierService    = `clair.projectquay.io/template-notifier-service`
	TemplateNotifierDeployment = `clair.projectquay.io/template-notifier-deployment`
)

// Pod port names.
//
// These are defined here because various components expect to be able to
// introspect ports by name.
const (
	PortAPI           = `api`
	PortIntrospection = `introspection`
)

func findCondition(cs []metav1.Condition, t string) ([]metav1.Condition, *metav1.Condition) {
	var c *metav1.Condition
	for i := range cs {
		if cs[i].Type == t {
			c = &cs[i]
			break
		}
	}
	if c == nil {
		l := len(cs)
		cs = append(cs, metav1.Condition{
			Type:               t,
			Status:             metav1.ConditionUnknown,
			LastTransitionTime: metav1.Now(),
		})
		c = &cs[l]
	}
	return cs, c
}

// ConfigReference is a reference to a ConfigMap or Secret resource with the
// `clair.projectquay.io/config` label.
type ConfigReference corev1.TypedLocalObjectReference

// ConfigMapReference is a reference to a ConfigMap.
type ConfigMapReference corev1.LocalObjectReference

// ServiceReference is a reference to a Service.
//
// If given a choice of arbitrary URI or a ServiceReference in an API, the
// latter should be preferred.
type ServiceReference struct {
	corev1.LocalObjectReference `json:",inline"`

	// Port ...
	// Defaults to 443.
	// +optional
	Port *int32 `json:"port,omitempty"`
}

func (r *ServiceReference) From(s *corev1.Service) error {
	switch {
	case s == nil:
		return errors.New("nil Service")
	case s.Name == "":
		return errors.New("Service object missing Name")
	case len(s.Spec.Ports) == 0:
		return errors.New("no Ports defined on Service")
	}

	if r == nil {
		r = new(ServiceReference)
	}
	r.Name = s.Name
	r.Port = &s.Spec.Ports[0].Port
	return nil
}

type DeploymentReference corev1.LocalObjectReference

func (r *DeploymentReference) From(d *appsv1.Deployment) error {
	switch {
	case d == nil:
		return errors.New("nil Deployment")
	case d.Name == "":
		return errors.New("Deployment object missing Name")
	}

	if r == nil {
		r = new(DeploymentReference)
	}
	r.Name = d.Name
	return nil
}

type AutoscalerReference corev1.LocalObjectReference

func (r *AutoscalerReference) From(a *scalev2.HorizontalPodAutoscaler) error {
	switch {
	case a == nil:
		return errors.New("nil HorizontalPodAutoscaler")
	case a.Name == "":
		return errors.New("HorizontalPodAutoscaler object missing Name")
	}

	if r == nil {
		r = new(AutoscalerReference)
	}
	r.Name = a.Name
	return nil
}

// ClientCert is a reference to a Secret of type `kubernetes.io/tls`, to be used
// as a client certificate.
type ClientCert corev1.SecretReference

// RefURI ...
type RefURI struct {
	// +optional
	URI *string `json:"uri,omitempty"`
	// +optional
	Secret *corev1.SecretKeySelector `json:"secretRef,omitempty"`
}
