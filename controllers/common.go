package controllers

import (
	"k8s.io/apimachinery/pkg/apis/meta/v1/unstructured"
	"k8s.io/apimachinery/pkg/runtime/schema"
)

const (
	ServiceSelectorKey = `clair.projectquay.io/service-kind`
	GroupSelectorKey   = `clair.projectquay.io/service-group`
)

type optionalTypes struct {
	Routes  bool
	HPA     bool
	Monitor bool
	Ingress bool
	Proxy   bool
}

func (o *optionalTypes) Set(gvk schema.GroupVersionKind) {
	switch gvk.Kind {
	case "Route":
		o.Routes = true
	case "HorizontalPodAutoscaler":
		o.HPA = true
	case "ServiceMonitor":
		o.Monitor = true
	case "Ingress":
		o.Ingress = true
	case "Proxy":
		o.Proxy = true
	default: // do nothing
	}
}

const (
	deployRecreate = `clair.projectquay.io/modifiedAt`
)

type configObject *unstructured.Unstructured
