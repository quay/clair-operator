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

// IndexerSpec defines the desired state of Indexer
type IndexerSpec struct {
	ServiceSpec `json:",inline"`
}

// IndexerStatus defines the observed state of Indexer
type IndexerStatus struct {
	ServiceStatus `json:",inline"`
}

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
