package controllers

import (
	routev1 "github.com/openshift/api/route/v1"
	monitorv1 "github.com/prometheus-operator/prometheus-operator/pkg/apis/monitoring/v1"
	scalev2 "k8s.io/api/autoscaling/v2beta2"
	netv1 "k8s.io/api/networking/v1"
	metav1 "k8s.io/apimachinery/pkg/apis/meta/v1"
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
}

var (
	routeGVK   = (&routev1.Route{}).GroupVersionKind()
	hpaGVK     = (&scalev2.HorizontalPodAutoscaler{}).GroupVersionKind()
	monitorGVK = (&monitorv1.ServiceMonitor{}).GroupVersionKind()
	ingressGVK = (&netv1.Ingress{}).GroupVersionKind()
)

func (o *optionalTypes) Set(gvk schema.GroupVersionKind) {
	switch gvk {
	case routeGVK:
		o.Routes = true
	case hpaGVK:
		o.HPA = true
	case monitorGVK:
		o.Monitor = true
	case ingressGVK:
		o.Ingress = true
	default: // do nothing
	}
}

const (
	deployRecreate = `clair.projectquay.io/modifiedAt`
)

func mkMeta(srv string, cur *metav1.ObjectMeta) metav1.ObjectMeta {
	return metav1.ObjectMeta{
		Namespace:    cur.Namespace,
		GenerateName: srv + "-",
		OwnerReferences: []metav1.OwnerReference{
			{Name: cur.Name, Controller: new(bool)},
		},
		Labels: map[string]string{
			ServiceSelectorKey: srv,
			GroupSelectorKey:   cur.Name,
		},
	}
}
