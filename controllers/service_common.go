package controllers

import (
	"context"
	"fmt"

	"github.com/go-logr/logr"
	monitorv1 "github.com/prometheus-operator/prometheus-operator/pkg/apis/monitoring/v1"
	appsv1 "k8s.io/api/apps/v1"
	scalev2 "k8s.io/api/autoscaling/v2beta2"
	corev1 "k8s.io/api/core/v1"
	metav1 "k8s.io/apimachinery/pkg/apis/meta/v1"
	"k8s.io/apimachinery/pkg/apis/meta/v1/unstructured"
	"k8s.io/apimachinery/pkg/runtime"
	"k8s.io/apimachinery/pkg/types"
	ctrl "sigs.k8s.io/controller-runtime"
	"sigs.k8s.io/controller-runtime/pkg/builder"
	"sigs.k8s.io/controller-runtime/pkg/client"
	"sigs.k8s.io/controller-runtime/pkg/handler"
	"sigs.k8s.io/controller-runtime/pkg/predicate"
	"sigs.k8s.io/controller-runtime/pkg/source"

	clairv1alpha1 "github.com/quay/clair-operator/api/v1alpha1"
)

// ServiceReconciler is common struct for the service reconciler loops.
type ServiceReconciler struct {
	client.Client
	Log     logr.Logger
	Scheme  *runtime.Scheme
	options optionalTypes
}

// SetupService sets up the controller with the Manager.
func (r *ServiceReconciler) SetupService(mgr ctrl.Manager, apiType client.Object) (*builder.Builder, error) {
	b := ctrl.NewControllerManagedBy(mgr).
		For(apiType).
		Owns(&appsv1.Deployment{}).
		Owns(&corev1.Service{}).
		// Do this manually for Secrets and ConfigMaps, because otherwise we
		// won't get events, as we're not the sole controller.
		Watches(&source.Kind{Type: &corev1.Secret{}},
			&handler.EnqueueRequestForOwner{OwnerType: apiType, IsController: false},
			builder.WithPredicates(&predicate.GenerationChangedPredicate{})).
		Watches(&source.Kind{Type: &corev1.ConfigMap{}},
			&handler.EnqueueRequestForOwner{OwnerType: apiType, IsController: false},
			builder.WithPredicates(&predicate.GenerationChangedPredicate{}))

	// Attempt to resolve some GVKs. If we can, this means they're installed and
	// we can use them.
	for _, obj := range []client.Object{
		&scalev2.HorizontalPodAutoscaler{},
		&monitorv1.ServiceMonitor{},
	} {
		gvk := obj.GetObjectKind().GroupVersionKind()
		if !r.Scheme.Recognizes(gvk) {
			r.Log.Info("missing optionally supported resource", "gvk", gvk.String())
			continue
		}
		b = b.Owns(obj)
		r.Log.Info("found optional kind", "gvk", gvk.String())
		r.options.Set(gvk)
	}
	return b, nil
}

func (r *ServiceReconciler) config(ctx context.Context, ns string, ref *clairv1alpha1.ConfigReference) (*unstructured.Unstructured, error) {
	var cfg unstructured.Unstructured
	name := types.NamespacedName{
		Namespace: ns,
		Name:      ref.Name,
	}
	if err := r.Client.Get(ctx, name, &cfg); err != nil {
		return nil, err
	}
	kind := cfg.GroupVersionKind().Kind
	if want := ref.Kind; kind != want {
		return nil, fmt.Errorf("unknown type pointed to by configReference: %q; wanted %q", kind, want)
	}
	if kind != "Secret" && kind != "ConfigMap" {
		return nil, fmt.Errorf("incorrect type pointed to by configReference: %q", kind)
	}
	return &cfg, nil
}

func conditionMap(cs []metav1.Condition, ts []string) map[string]metav1.ConditionStatus {
	m := make(map[string]metav1.ConditionStatus, len(ts))
	for _, t := range ts {
		m[t] = metav1.ConditionUnknown
	}
	for _, c := range cs {
		if _, ok := m[c.Type]; ok {
			m[c.Type] = c.Status
		}
	}
	return m
}
