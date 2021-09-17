package controllers

import (
	"context"
	"encoding/json"
	"fmt"

	"github.com/go-logr/logr"
	monitorv1 "github.com/prometheus-operator/prometheus-operator/pkg/apis/monitoring/v1"
	appsv1 "k8s.io/api/apps/v1"
	scalev2 "k8s.io/api/autoscaling/v2beta2"
	corev1 "k8s.io/api/core/v1"
	metav1 "k8s.io/apimachinery/pkg/apis/meta/v1"
	"k8s.io/apimachinery/pkg/apis/meta/v1/unstructured"
	"k8s.io/apimachinery/pkg/runtime"
	"k8s.io/apimachinery/pkg/runtime/schema"
	"k8s.io/apimachinery/pkg/types"
	ctrl "sigs.k8s.io/controller-runtime"
	"sigs.k8s.io/controller-runtime/pkg/builder"
	"sigs.k8s.io/controller-runtime/pkg/client"
	"sigs.k8s.io/controller-runtime/pkg/controller/controllerutil"
	"sigs.k8s.io/controller-runtime/pkg/handler"
	logf "sigs.k8s.io/controller-runtime/pkg/log"
	"sigs.k8s.io/controller-runtime/pkg/predicate"
	"sigs.k8s.io/controller-runtime/pkg/source"

	clairv1alpha1 "github.com/quay/clair-operator/api/v1alpha1"
)

// ServiceReconciler is common struct for the service reconciler loops.
type ServiceReconciler struct {
	client.Client
	Log     logr.Logger
	Scheme  *runtime.Scheme
	k       *kustomize
	options optionalTypes
}

// SetupService sets up the controller with the Manager.
func (r *ServiceReconciler) SetupService(mgr ctrl.Manager, apiType client.Object) (*builder.Builder, error) {
	k, err := newKustomize()
	if err != nil {
		return nil, err
	}
	r.k = k
	r.Client = mgr.GetClient()
	r.Scheme = mgr.GetScheme()
	b := ctrl.NewControllerManagedBy(mgr).
		WithLogger(r.Log).
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
	for _, pair := range []struct {
		obj client.Object
		gvk schema.GroupVersionKind
	}{
		{
			&scalev2.HorizontalPodAutoscaler{},
			schema.GroupVersionKind{
				Group: "autoscaling", Version: "v2beta2", Kind: "HorizontalPodAutoscaler",
			},
		},
		{
			&monitorv1.ServiceMonitor{},
			schema.GroupVersionKind{
				Group: "monitoring.coreos.com", Version: "v1", Kind: "ServiceMonitor",
			},
		},
	} {
		if !r.Scheme.Recognizes(pair.gvk) {
			r.Log.Info("missing optionally supported resource", "gvk", pair.gvk.String())
			continue
		}
		b = b.Owns(pair.obj)
		r.Log.Info("found optional kind", "gvk", pair.gvk.String())
		r.options.Set(pair.gvk)
	}
	return b, nil
}

func (r *ServiceReconciler) config(ctx context.Context, ns string, ref *clairv1alpha1.ConfigReference) (*unstructured.Unstructured, error) {
	log := logf.FromContext(ctx)
	log.V(1).Info("looking up ref", "kind", ref.Kind, "name", ref.Name)
	if ref.Kind != "Secret" && ref.Kind != "ConfigMap" {
		return nil, fmt.Errorf("incorrect type pointed to by configReference: %q", ref.Kind)
	}

	var cfg unstructured.Unstructured
	cfg.SetGroupVersionKind(schema.GroupVersionKind{
		Version: "v1",
		Kind:    ref.Kind,
	})
	name := types.NamespacedName{
		Namespace: ns,
		Name:      ref.Name,
	}
	if err := r.Client.Get(ctx, name, &cfg); err != nil {
		return nil, err
	}
	kind := cfg.GetKind()
	if want := ref.Kind; kind != want {
		return nil, fmt.Errorf("unknown type pointed to by configReference: %q; wanted %q", kind, want)
	}
	return &cfg, nil
}

func (r *ServiceReconciler) CheckRefsAvailable(ctx context.Context, cur client.Object, refs []corev1.TypedLocalObjectReference) (metav1.Condition, error) {
	log := logf.FromContext(ctx)
	rc := metav1.Condition{
		Type:               clairv1alpha1.ServiceAvailable,
		Status:             metav1.ConditionFalse,
		ObservedGeneration: cur.GetGeneration(),
		LastTransitionTime: metav1.Now(),
	}
	n := types.NamespacedName{
		Namespace: cur.GetNamespace(),
	}
	for _, ref := range refs {
		var ready bool
		var reason string
		n.Name = ref.Name
		switch ref.Kind {
		case "Deployment":
			reason = `DeploymentUnavailable`
			var d appsv1.Deployment
			if err := r.Client.Get(ctx, n, &d); err != nil {
				rc.Reason = reason
				rc.Message = err.Error()
				return rc, err
			}
			for _, cnd := range d.Status.Conditions {
				log.V(1).Info("examining Deployment", "name", d.Name, "condition", cnd)
				if cnd.Type == appsv1.DeploymentAvailable && cnd.Status == corev1.ConditionTrue {
					ready = true
					break
				}
			}
		case "Service":
			// Services are always OK
			ready = true
		default:
			log.V(1).Info("skipping ref", "kind", ref.Kind, "name", ref.Name)
			continue
		}
		if !ready {
			rc.Reason = reason
			log.V(1).Info("not ready", "condition", rc)
			return rc, nil
		}
	}
	rc.Status = metav1.ConditionTrue
	rc.Reason = `RefsAvailable`
	return rc, nil
}

func (r *ServiceReconciler) InflateTemplates(ctx context.Context, cur, next client.Object, cfg *unstructured.Unstructured) (ctrl.Result, error) {
	log := logf.FromContext(ctx)

	cfgAnno := cfg.GetAnnotations()
	if cfgAnno == nil {
		cfgAnno = make(map[string]string)
	}
	status := getStatus(next)

	res, err := r.k.Run(cfg, templateName(cur), clairImage)
	if err != nil {
		return ctrl.Result{}, err
	}
	var (
		deploy  appsv1.Deployment
		srv     corev1.Service
		hpa     scalev2.HorizontalPodAutoscaler
		monitor monitorv1.ServiceMonitor
	)
	for _, tmpl := range res.Resources() {
		tmpl.SetNamespace(cur.GetNamespace())
		b, err := tmpl.MarshalJSON()
		if err != nil {
			return ctrl.Result{}, err
		}
		log.Info("resource", "res", tmpl.GetKind()+"/"+tmpl.GetName())
		switch tmpl.GetKind() {
		case "Deployment":
			if err := json.Unmarshal(b, &deploy); err != nil {
				return ctrl.Result{}, err
			}
			if err := controllerutil.SetControllerReference(cur, &deploy, r.Scheme); err != nil {
				return ctrl.Result{}, err
			}
		case "Service":
			if err := json.Unmarshal(b, &srv); err != nil {
				return ctrl.Result{}, err
			}
			if err := controllerutil.SetControllerReference(cur, &srv, r.Scheme); err != nil {
				return ctrl.Result{}, err
			}
		case "HorizontalPodAutoscaler":
			if err := json.Unmarshal(b, &hpa); err != nil {
				return ctrl.Result{}, err
			}
			if err := controllerutil.SetControllerReference(cur, &hpa, r.Scheme); err != nil {
				return ctrl.Result{}, err
			}
		case "ServiceMonitor":
			if err := json.Unmarshal(b, &monitor); err != nil {
				return ctrl.Result{}, err
			}
			if err := controllerutil.SetControllerReference(cur, &monitor, r.Scheme); err != nil {
				return ctrl.Result{}, err
			}
		default:
			log.Info("unknown resource", "kind", tmpl.GetKind())
		}
	}

	// Create the deployment and touch anything that needs to know its name.
	if err := r.Client.Create(ctx, &deploy); err != nil {
		return ctrl.Result{}, err
	}
	if err := status.AddRef(&deploy, r.Scheme); err != nil {
		return ctrl.Result{}, err
	}
	if err := r.Client.Status().Update(ctx, next); err != nil {
		return ctrl.Result{}, err
	}
	cfgAnno[clairv1alpha1.TemplateMatcherDeployment] = deploy.Namespace + "/" + deploy.Name
	log.Info("created deployment", "ref", cfgAnno[clairv1alpha1.TemplateMatcherDeployment])

	// Create the service and anything that needs to know its name.
	if err := r.Client.Create(ctx, &srv); err != nil {
		return ctrl.Result{}, err
	}
	if err := status.AddRef(&srv, r.Scheme); err != nil {
		return ctrl.Result{}, err
	}
	if err := r.Client.Status().Update(ctx, next); err != nil {
		return ctrl.Result{}, err
	}
	cfgAnno[clairv1alpha1.TemplateMatcherService] = srv.Namespace + "/" + srv.Name
	log.Info("created service", "ref", srv.Namespace+"/"+srv.Name)

	if r.options.HPA {
		if err := r.Client.Create(ctx, &hpa); err != nil {
			return ctrl.Result{}, err
		}
		if err := status.AddRef(&hpa, r.Scheme); err != nil {
			return ctrl.Result{}, err
		}
		if err := r.Client.Status().Update(ctx, next); err != nil {
			return ctrl.Result{}, err
		}
		log.Info("created hpa", "ref", hpa.Namespace+"/"+hpa.Name)
	} else {
		log.V(1).Info("skipping hpa creation")
	}

	if r.options.Monitor {
		if err := r.Client.Create(ctx, &monitor); err != nil {
			return ctrl.Result{}, err
		}
		if err := status.AddRef(&monitor, r.Scheme); err != nil {
			return ctrl.Result{}, err
		}
		if err := r.Client.Status().Update(ctx, next); err != nil {
			return ctrl.Result{}, err
		}
		log.Info("created servicemonitor", "ref", monitor.Namespace+"/"+monitor.Name)
	} else {
		log.V(1).Info("skipping Monitor creation")
	}

	// Purposefully grab the current version number.
	//
	// Don't know if we'll see an update from the annotation changes.
	status.ConfigVersion = cfg.GetResourceVersion()
	// Add a non-controlling owner ref so that we get notifications when things
	// change.
	if err := controllerutil.SetOwnerReference(cur, cfg, r.Scheme); err != nil {
		return ctrl.Result{}, err
	}
	if err := r.Client.Update(ctx, cfg); err != nil {
		return ctrl.Result{}, err
	}
	log.Info("config updated")
	if err := r.Client.Status().Update(ctx, next); err != nil {
		return ctrl.Result{}, err
	}
	log.Info("indexer updated")

	return ctrl.Result{}, nil
}

// GetSpec pulls the common spec struct out of the enclosing types.
func getSpec(cur client.Object) *clairv1alpha1.ServiceSpec {
	switch r := cur.(type) {
	case *clairv1alpha1.Matcher:
		return &r.Spec.ServiceSpec
	case *clairv1alpha1.Indexer:
		return &r.Spec.ServiceSpec
	case *clairv1alpha1.Notifier:
		return &r.Spec.ServiceSpec
	default:
	}
	panic(fmt.Sprintf("programmer error: called with unexpected type: %T", cur))
}

// GetStatus pulls the common status struct out of the enclosing types.
func getStatus(cur client.Object) *clairv1alpha1.ServiceStatus {
	switch r := cur.(type) {
	case *clairv1alpha1.Matcher:
		return &r.Status.ServiceStatus
	case *clairv1alpha1.Indexer:
		return &r.Status.ServiceStatus
	case *clairv1alpha1.Notifier:
		return &r.Status.ServiceStatus
	default:
	}
	panic(fmt.Sprintf("programmer error: called with unexpected type: %T", cur))
}

// TemplateName returns the name of the embedded templates for the passed
// struct.
func templateName(cur client.Object) string {
	switch cur.(type) {
	case *clairv1alpha1.Matcher:
		return "matcher"
	case *clairv1alpha1.Indexer:
		return "indexer"
	case *clairv1alpha1.Notifier:
		return "notifier"
	default:
	}
	panic(fmt.Sprintf("programmer error: called with unexpected type: %T", cur))
}

func (s *ServiceReconciler) CheckResources(ctx context.Context, cur, next client.Object, cfg *unstructured.Unstructured) (ctrl.Result, error) {
	log := logf.FromContext(ctx)
	log.Info("checking resources")
	// Check annotations
	a := cfg.GetAnnotations()
	if a == nil {
		a = make(map[string]string)
	}
	status := getStatus(cur)
	var (
		deployName string
		deployAnno = deploymentAnnotation(cur)
		srvName    string
		srvAnno    = serviceAnnotation(cur)
		changed    bool
	)
	for _, r := range status.Refs {
		switch r.Kind {
		case "Deployment":
			deployName = cur.GetNamespace() + "/" + r.Name
		case "Service":
			srvName = cur.GetNamespace() + "/" + r.Name
		}
	}
	switch {
	case deployName == "":
		log.Info("missing deployment")
	case a[deployAnno] != deployName:
		log.Info("updating configuration deployment")
		changed = true
		a[deployAnno] = deployName
	}
	switch {
	case srvName == "":
		log.Info("missing service")
	case a[srvAnno] != srvName:
		log.Info("updating configuration service")
		changed = true
		a[srvAnno] = srvName
	}
	if changed {
		cfg.SetAnnotations(a)
		if err := s.Client.Update(ctx, cfg); err != nil {
			return ctrl.Result{}, err
		}
		log.Info("updated config", "name", cfg.GetName())
		return ctrl.Result{Requeue: true}, nil
	}

	var curStatus *metav1.Condition
	for _, s := range status.Conditions {
		if s.Type == clairv1alpha1.ServiceAvailable {
			curStatus = &s
			break
		}
	}
	// Check deployment Status
	log.Info("checking refs")
	cnd, err := s.CheckRefsAvailable(ctx, cur, status.Refs)
	if err != nil {
		return ctrl.Result{}, err
	}
	ns := getStatus(next)
	switch {
	case curStatus == nil:
		ns.Conditions = append(ns.Conditions, cnd)
		if err := s.Client.Status().Update(ctx, next); err != nil {
			return ctrl.Result{}, err
		}
	case curStatus.Reason != cnd.Reason:
		log.V(1).Info("updating: dependent resources changed", "condition", cnd)
		for i, sc := range ns.Conditions {
			if sc.Type == cnd.Type {
				cnd.DeepCopyInto(&ns.Conditions[i])
				break
			}
		}
		if err := s.Client.Status().Update(ctx, next); err != nil {
			return ctrl.Result{}, err
		}
	case curStatus.Reason == cnd.Reason:
		log.V(1).Info("skipping update: dependent resources unchanged")
	}

	return ctrl.Result{}, nil
}

// DeploymentAnnotation returns the correct annotation for the deployment of the
// type passed in.
func deploymentAnnotation(cur client.Object) string {
	switch cur.(type) {
	case *clairv1alpha1.Matcher:
		return clairv1alpha1.TemplateMatcherDeployment
	case *clairv1alpha1.Indexer:
		return clairv1alpha1.TemplateIndexerDeployment
	case *clairv1alpha1.Notifier:
		return clairv1alpha1.TemplateNotifierDeployment
	default:
	}
	panic(fmt.Sprintf("programmer error: called with unexpected type: %T", cur))
}

// ServiceAnnotation returns the correct annotation for the service of the
// type passed in.
func serviceAnnotation(cur client.Object) string {
	switch cur.(type) {
	case *clairv1alpha1.Matcher:
		return clairv1alpha1.TemplateMatcherService
	case *clairv1alpha1.Indexer:
		return clairv1alpha1.TemplateIndexerService
	case *clairv1alpha1.Notifier:
		return clairv1alpha1.TemplateNotifierService
	default:
	}
	panic(fmt.Sprintf("programmer error: called with unexpected type: %T", cur))
}
