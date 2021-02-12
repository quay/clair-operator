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

// IndexerSpec defines the desired state of Indexer
type IndexerSpec struct {
	// INSERT ADDITIONAL SPEC FIELDS - desired state of cluster
	// Important: Run "make" to regenerate code after modifying this file

	// Config ...
	Config *ConfigReference `json:"configRef,omitempty"`
}

// IndexerStatus defines the observed state of Indexer
type IndexerStatus struct {
	// INSERT ADDITIONAL STATUS FIELD - define observed state of cluster
	// Important: Run "make" to regenerate code after modifying this file

	// Represents the observations of an Indexer's current state.
	// Known .status.conditions.type are: "Available", "Progressing"
	// +patchMergeKey=type
	// +patchStrategy=merge
	// +listType=map
	// +listMapKey=type
	Conditions []metav1.Condition `json:"conditions,omitempty" patchStrategy:"merge" patchMergeKey:"type"`

	Deployment    *DeploymentReference `json:"deploymentRef,omitempty"`
	Service       *ServiceReference    `json:"serviceRef,omitempty"`
	Autoscaler    *AutoscalerReference `json:"autoscalerRef,omitempty"`
	ConfigVersion string               `json:"configVersion,omitempty"`
}

const (
	IndexerAvailable           = `Available`
	ServiceRedeploying         = `clair.projectquay.io/Redeploying`
	IndexerServiceAvailable    = `clair.projectquay.io/ServiceAvailable`
	IndexerDeploymentAvailable = `clair.projectquay.io/DeploymentAvailable`
)

type IndexerConditionReason string

// Available reasons:
const (
	IndexerReasonEmpty             IndexerConditionReason = `Empty`
	IndexerReasonServiceCreated    IndexerConditionReason = `ServiceCreated`
	IndexerReasonDeploymentCreated IndexerConditionReason = `DeploymentCreated`
	IndexerReasonSteady            IndexerConditionReason = `Steady`
	IndexerReasonRedeploying       IndexerConditionReason = `Redeploying`
)

const (
	IndexerReasonNeedService IndexerConditionReason = `NeedService`
)

// GetCondition returns a Condition associated with the provided type, adding an
// entry to the backing slice if necessary.
func (i *Indexer) GetCondition(t string) (c *metav1.Condition) {
	i.Status.Conditions, c = findCondition(i.Status.Conditions, t)
	return c
}

// +kubebuilder:object:root=true
// +kubebuilder:subresource:status

// Indexer is the Schema for the indexers API
type Indexer struct {
	metav1.TypeMeta   `json:",inline"`
	metav1.ObjectMeta `json:"metadata,omitempty"`

	Spec   IndexerSpec   `json:"spec,omitempty"`
	Status IndexerStatus `json:"status,omitempty"`
}

// +kubebuilder:object:root=true

// IndexerList contains a list of Indexer
type IndexerList struct {
	metav1.TypeMeta `json:",inline"`
	metav1.ListMeta `json:"metadata,omitempty"`
	Items           []Indexer `json:"items"`
}

func init() {
	SchemeBuilder.Register(&Indexer{}, &IndexerList{})
}
