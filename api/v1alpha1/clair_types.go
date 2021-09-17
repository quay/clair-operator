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
	"fmt"

	corev1 "k8s.io/api/core/v1"
	metav1 "k8s.io/apimachinery/pkg/apis/meta/v1"
	runtime "k8s.io/apimachinery/pkg/runtime"
	"sigs.k8s.io/controller-runtime/pkg/client/apiutil"
)

// ClairSpec defines the desired state of Clair.
type ClairSpec struct {
	// INSERT ADDITIONAL SPEC FIELDS - desired state of cluster
	// Important: Run "make" to regenerate code after modifying this file

	// DatabaseURIs indicates the unmanaged databases that services should
	// connect to.
	//
	// If not provided, a database engine will be started and used.
	// +optional
	Databases *Databases `json:"databases,omitempty"`

	// Notifier ...
	//
	// If not provided, a notifier will not be provisioned.
	// +optional
	Notifier *NotifierConfig `json:"notifier,omitempty"`
}

// Databases ...
type Databases struct {
	// +kubebuilder:validation:Required

	// Indexer ...
	Indexer *RefURI `json:"indexer,omitempty"`

	// +kubebuilder:validation:Required

	// Matcher ...
	Matcher *RefURI `json:"matcher,omitempty"`

	// +kubebuilder:validation:Required

	// Notifier ...
	Notifier *RefURI `json:"notifier,omitempty"`
}

type NotifierConfig struct {
	// +optional
	Webhook *WebhookNotifier `json:"webook,omitempty"`
	// +optional
	AMQP *AMQPNotifier `json:"amqp,omitempty"`
	// +optional
	STOMP *STOMPNotifier `json:"stomp,omitempty"`
}

type WebhookNotifier struct {
	// URL ...
	URL string `json:"url"`

	// Headers ...
	// +optional
	Headers []Header `json:"headers,omitempty"`

	// Sign ...
	// +optional
	Sign bool `json:"sign,omitempty"`
}

type Header struct {
	Name  string   `json:"name"`
	Value []string `json:"value"`
}

type STOMPNotifier struct {
	// ClientCert ...
	// +optional
	ClientCert *ClientCert `json:"clientCert,omitempty"`

	// Login ...
	// +optional
	Login *STOMPLogin `json:"login,omitempty"`

	// Rollup ...
	// +optional
	Rollup *int32 `json:"rollup,omitempty"`

	Destination string   `json:"destination"`
	URIs        []string `json:"uris"`
}

type STOMPLogin struct {
	// Login ...
	// Required
	Login string `json:"login"`
	// Passcode ...
	// Required
	Passcode string `json:"passcode"`
}

type AMQPNotifier struct {
	// Rollup ...
	// +optional
	Rollup *int32 `json:"rollup,omitempty"`

	// ClientCert ...
	// +optional
	ClientCert *ClientCert `json:"clientCert,omitempty"`

	URIs       []string     `json:"uris"`
	RoutingKey string       `json:"routingKey"`
	Exchange   AMQPExchange `json:"exchange"`
}

type AMQPExchange struct {
	Name string `json:"name"`

	// +kubebuilder:validation:Enum=direct;fanout;topic;headers

	Type string `json:"type"`

	Durable bool `json:"durable"`

	AutoDelete bool `json:"autoDelete"`
}

// ClairStatus defines the observed state of Clair
type ClairStatus struct {
	// INSERT ADDITIONAL STATUS FIELD - define observed state of cluster
	// Important: Run "make" to regenerate code after modifying this file

	// Conditions ...
	//
	// +patchMergeKey=type
	// +patchStrategy=merge
	// +listType=map
	// +listMapKey=type
	Conditions []metav1.Condition `json:"conditions,omitempty" patchStrategy:"merge" patchMergeKey:"type"`

	// Refs ...
	//
	// +patchMergeKey=name
	// +patchStrategy=merge
	// +listType=map
	// +listMapKey=name
	Refs []corev1.TypedLocalObjectReference `json:"refs,omitempty" patchStrategy:"merge" patchMergeKey:"name"`

	// The below are set by the operator as things are configured and ready.

	// Endpoint ...
	Endpoint string `json:"endpoint,omitempty"`
	// Config ...
	Config *ConfigReference `json:"config,omitempty"`
	// Database ...
	Database *ServiceRef `json:"database,omitempty"`
	// Indexer ...
	Indexer *ServiceRef `json:"indexer,omitempty"`
	// Matcher ...
	Matcher *ServiceRef `json:"matcher,omitempty"`
	// Notifier ...
	Notifier *ServiceRef `json:"notifier,omitempty"`
}

func (s *ClairStatus) AddRef(obj metav1.Object, scheme *runtime.Scheme) error {
	ro, ok := obj.(runtime.Object)
	if !ok {
		return fmt.Errorf("%T is not a runtime.Object", obj)
	}
	gvk, err := apiutil.GVKForObject(ro, scheme)
	if err != nil {
		return err
	}
	s.Refs = append(s.Refs, corev1.TypedLocalObjectReference{
		APIGroup: &gvk.Group,
		Kind:     gvk.Kind,
		Name:     obj.GetName(),
	})
	return nil
}

// A ServiceRef is a paired Deployment and Service.
type ServiceRef struct {
	Deployment *DeploymentReference `json:"deployment,omitempty"`
	Service    *ServiceReference    `json:"service,omitempty"`
}

func (r *ServiceRef) Populated() bool {
	return r != nil && r.Service != nil && r.Deployment != nil
}

// +kubebuilder:object:root=true
// +kubebuilder:subresource:status

// Clair is the Schema for the clairs API
type Clair struct {
	metav1.TypeMeta   `json:",inline"`
	metav1.ObjectMeta `json:"metadata,omitempty"`

	Spec   ClairSpec   `json:"spec,omitempty"`
	Status ClairStatus `json:"status,omitempty"`
}

// +kubebuilder:object:root=true

// ClairList contains a list of Clair
type ClairList struct {
	metav1.TypeMeta `json:",inline"`
	metav1.ListMeta `json:"metadata,omitempty"`
	Items           []Clair `json:"items"`
}

func init() {
	SchemeBuilder.Register(&Clair{}, &ClairList{})
}
