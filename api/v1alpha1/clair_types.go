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
	metav1 "k8s.io/apimachinery/pkg/apis/meta/v1"
)

// EDIT THIS FILE!  THIS IS SCAFFOLDING FOR YOU TO OWN!
// NOTE: json tags are required.  Any new fields you add must have json tags for the fields to be serialized.

// ClairSpec defines the desired state of Clair
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
	// +optional
	Notifier *NotifierConfig `json:"notifier,omitempty"`
}

// Databases indicates where
type Databases struct {
	// +kubebuilder:validation:Required

	// Indexer ...
	Indexer RefURI `json:"indexer"`

	// +kubebuilder:validation:Required

	// Matcher ...
	Matcher RefURI `json:"matcher"`

	// +kubebuilder:validation:Required

	// Notifier ...
	Notifier RefURI `json:"notifier"`
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
	URIs        []string `json:"uris"`
	Destination string   `json:"destination"`

	// Rollup ...
	// +optional
	Rollup *int32 `json:"rollup,omitempty"`

	// ClientCert ...
	// +optional
	ClientCert *ClientCert `json:"clientCert,omitempty"`

	// Login ...
	// +optional
	Login *STOMPLogin `json:"login,omitempty"`
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
	URIs       []string     `json:"uris"`
	RoutingKey string       `json:"routingKey"`
	Exchange   AMQPExchange `json:"exchange"`

	// Rollup ...
	// +optional
	Rollup *int32 `json:"rollup,omitempty"`

	// ClientCert ...
	// +optional
	ClientCert *ClientCert `json:"clientCert,omitempty"`
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

	// Condition ...
	Condition []ClairCondition `json:"condition,omitempty"`

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

func (c *Clair) SetCondition(t ClairConditionType, s metav1.ConditionStatus, r ClairConditionReason, msg string) {
	now := metav1.Now()
	cs := c.Status.Condition
	var cnd *ClairCondition
	for i := range cs {
		if cs[i].Type == t {
			cnd = &cs[i]
			break
		}
	}
	if cnd == nil {
		c.Status.Condition = append(c.Status.Condition, ClairCondition{
			Type:   t,
			Status: metav1.ConditionUnknown,
		})
	}

	if cnd.Status != s {
		cnd.Status = s
		cnd.LastTransitionTime = &now
	}
	if s == metav1.ConditionTrue && *cnd.Reason != r {
		cnd.Reason = &r
		cnd.LastUpdateTime = &now
	}
	if msg != "" {
		cnd.Message = &msg
		cnd.LastUpdateTime = &now
	}
}

func (c *Clair) GetCondition(t ClairConditionType) *ClairCondition {
	cs := c.Status.Condition
	for i := range cs {
		if cs[i].Type == t {
			return &cs[i]
		}
	}
	return nil
}

type ClairCondition struct {
	Type   ClairConditionType     `json:"type"`
	Status metav1.ConditionStatus `json:"status"`

	// +optional
	Reason *ClairConditionReason `json:"reason,omitempty"`
	// +optional
	Message *string `json:"message,omitempty"`

	// +optional
	LastUpdateTime *metav1.Time `json:"lastUpdateTime,omitempty"`
	// +optional
	LastTransitionTime *metav1.Time `json:"lastTransitionTime,omitempty"`
}

type ClairConditionType string

const (
	ClairAvailable     ClairConditionType = "Available"
	ClairConfigBlocked ClairConditionType = "ConfigurationBlocked"
)

type ClairConditionReason string

const (
	ClairReasonHealthChecksPassing ClairConditionReason = "HealthChecksPassing"

	ClairReasonMissingDeps ClairConditionReason = "DependenciesMissing"
)

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
