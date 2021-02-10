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

package controllers

import (
	"context"
	"fmt"
	"path/filepath"
	"time"

	"github.com/go-logr/logr"
	monitorv1 "github.com/prometheus-operator/prometheus-operator/pkg/apis/monitoring/v1"
	appsv1 "k8s.io/api/apps/v1"
	scalev2 "k8s.io/api/autoscaling/v2beta2"
	corev1 "k8s.io/api/core/v1"
	k8serr "k8s.io/apimachinery/pkg/api/errors"
	metav1 "k8s.io/apimachinery/pkg/apis/meta/v1"
	"k8s.io/apimachinery/pkg/apis/meta/v1/unstructured"
	"k8s.io/apimachinery/pkg/runtime"
	"k8s.io/apimachinery/pkg/types"
	"k8s.io/apimachinery/pkg/util/intstr"
	ctrl "sigs.k8s.io/controller-runtime"
	"sigs.k8s.io/controller-runtime/pkg/builder"
	"sigs.k8s.io/controller-runtime/pkg/client"
	"sigs.k8s.io/controller-runtime/pkg/handler"
	"sigs.k8s.io/controller-runtime/pkg/predicate"
	"sigs.k8s.io/controller-runtime/pkg/source"

	clairv1alpha1 "github.com/quay/clair-operator/api/v1alpha1"
)

// IndexerReconciler reconciles a Indexer object
type IndexerReconciler struct {
	client.Client
	Log     logr.Logger
	Scheme  *runtime.Scheme
	options optionalTypes
}

// +kubebuilder:rbac:groups=clair.projectquay.io,resources=indexers,verbs=get;list;watch;create;update;patch;delete
// +kubebuilder:rbac:groups=clair.projectquay.io,resources=indexers/status,verbs=get;update;patch
// +kubebuilder:rbac:groups=clair.projectquay.io,resources=indexers/finalizers,verbs=update

// Reconcile is part of the main kubernetes reconciliation loop which aims to
// move the current state of the cluster closer to the desired state.
func (r *IndexerReconciler) Reconcile(ctx context.Context, req ctrl.Request) (ctrl.Result, error) {
	log := r.Log.WithValues("indexer", req.NamespacedName)
	log.Info("start")
	defer log.Info("done")
	var (
		cur clairv1alpha1.Indexer
		res ctrl.Result
	)
	err := r.Client.Get(ctx, req.NamespacedName, &cur)
	switch {
	case err == nil:
	case k8serr.IsNotFound(err):
		// ???
		return res, nil
	default:
		return res, client.IgnoreNotFound(err)
	}
	next := cur.DeepCopy()

	checkConfig := true
	cnd := next.GetCondition(clairv1alpha1.IndexerAvailable)
	switch {
	case cnd.Status == metav1.ConditionUnknown:
		log.Info("initial run")
		// initial run
		if err := r.indexerTemplates(ctx, &cur, next); err != nil {
			return res, err
		}
		cnd.ObservedGeneration = cur.Generation
		cnd.Status = metav1.ConditionFalse
		cnd.Reason = "InitialCreation"
		checkConfig = false
	case cnd.Status == metav1.ConditionFalse:

	case cnd.Status == metav1.ConditionTrue:
		// check for dependency change
		//case cnd.Reason == clairv1alpha1.IndexerReasonServiceCreated:
		// create deployment
		//case cnd.Reason == clairv1alpha1.IndexerReasonDeploymentCreated:
		// wait on config
		//case cnd.Reason == clairv1alpha1.IndexerReasonRedeploying:
		// Mess with the deployment
	}
	if checkConfig {
		cfg, err := r.config(ctx, &cur)
		if err != nil {
			return res, err
		}
		cnd := next.GetCondition(clairv1alpha1.ServiceRedeploying)
		if v := cfg.GetResourceVersion(); v != cur.Status.ConfigVersion {
			d := appsv1.Deployment{}

			if err := r.Client.Get(ctx, types.NamespacedName{
				Name:      cur.Status.Deployment.Name,
				Namespace: cur.Namespace,
			}, &d); err != nil {
				return res, err
			}
			d.Annotations[deployRecreate] = time.Now().Format(time.RFC3339)
			if err := r.Client.Update(ctx, &d); err != nil {
				return res, err
			}
			next.Status.ConfigVersion = v
			cnd.Status = metav1.ConditionTrue
			cnd.Reason = `ConfigurationChanged`
		} else {
			cnd.Status = metav1.ConditionFalse
			cnd.Reason = `DeploymentUpdated`
		}
	}
	if err := r.Client.Update(ctx, next); err != nil {
		return res, err
	}
	return res, nil
}

func (r *IndexerReconciler) config(ctx context.Context, cur *clairv1alpha1.Indexer) (*unstructured.Unstructured, error) {
	var cfg unstructured.Unstructured
	name := types.NamespacedName{
		Namespace: cur.Namespace,
		Name:      cur.Spec.Config.Name,
	}
	if err := r.Client.Get(ctx, name, &cfg); err != nil {
		return nil, err
	}
	kind := cfg.GroupVersionKind().Kind
	if want := cur.Spec.Config.Kind; kind != want {
		return nil, fmt.Errorf("unknown type pointed to by configReference: %q; wanted %q", kind, want)
	}
	if kind != "Secret" && kind != "ConfigMap" {
		return nil, fmt.Errorf("incorrect type pointed to by configReference: %q", kind)
	}
	return &cfg, nil
}

func (r *IndexerReconciler) indexerTemplates(ctx context.Context, cur, next *clairv1alpha1.Indexer) error {
	const (
		serviceName  = `indexer`
		configVolume = `config`
		configFile   = `config.yaml`
		configMount  = `/run/config`

		// TODO(hank) Allow configuration, by environment variable?
		img = `quay.io/projectquay/clair:4.0.0`
	)
	var (
		now       = time.Now()
		selectors = map[string]string{
			ServiceSelectorKey: serviceName,
			GroupSelectorKey:   cur.Name,
		}
		log = r.Log.WithValues("indexer", types.NamespacedName{Name: cur.Name, Namespace: cur.Namespace})

		configSource corev1.VolumeSource
	)

	// Populate the config source for our container volume.
	cfg, err := r.config(ctx, cur)
	if err != nil {
		return err
	}
	cfgkey := configFile
	cfgAnno := cfg.GetAnnotations()
	if val, ok := cfgAnno[clairv1alpha1.ConfigAnnotation]; ok {
		cfgkey = val
	}
	items := []corev1.KeyToPath{{Key: cfgkey, Path: configFile}}
	switch cfg.GroupVersionKind().Kind {
	case "Secret":
		configSource.Secret = &corev1.SecretVolumeSource{
			SecretName: cfg.GetName(),
			Items:      items,
		}
	case "ConfigMap":
		configSource.ConfigMap = &corev1.ConfigMapVolumeSource{
			LocalObjectReference: corev1.LocalObjectReference{Name: cfg.GetName()},
			Items:                items,
		}
	default:
		panic("unreachable")
	}
	log.Info("found config", "ref", cur.Spec.Config)

	srv := corev1.Service{
		ObjectMeta: mkMeta(serviceName, &cur.ObjectMeta),
		Spec: corev1.ServiceSpec{
			Selector: selectors,
			Type:     corev1.ServiceTypeClusterIP,
			Ports: []corev1.ServicePort{
				{
					Name:       clairv1alpha1.PortAPI,
					Port:       80,
					TargetPort: intstr.FromString(clairv1alpha1.PortAPI),
				},
				{
					Name:       clairv1alpha1.PortIntrospection,
					Port:       8090,
					TargetPort: intstr.FromString(clairv1alpha1.PortIntrospection),
				},
			},
		},
	}
	deploy := appsv1.Deployment{
		ObjectMeta: mkMeta(serviceName, &cur.ObjectMeta),
		Spec: appsv1.DeploymentSpec{
			Selector: &metav1.LabelSelector{MatchLabels: selectors},
			Template: corev1.PodTemplateSpec{
				ObjectMeta: metav1.ObjectMeta{
					Labels:      selectors,
					Annotations: map[string]string{deployRecreate: now.Format(time.RFC3339)},
				},
				Spec: corev1.PodSpec{
					// TODO(hank) Add resource estimates.
					Containers: []corev1.Container{
						{
							Name:  "clair",
							Image: img,
							Env: []corev1.EnvVar{
								{Name: "CLAIR_MODE", Value: serviceName},
								{Name: "CLAIR_CONF", Value: filepath.Join(configMount, configFile)},
							},
							VolumeMounts: []corev1.VolumeMount{
								{Name: configVolume, MountPath: configMount, SubPath: configFile, ReadOnly: true},
							},
							Ports: []corev1.ContainerPort{
								{Name: clairv1alpha1.PortAPI, ContainerPort: 8080},
								{Name: clairv1alpha1.PortIntrospection, ContainerPort: 8089},
							},
							// TODO(hank) Add container probes.
						},
					},
					Volumes: []corev1.Volume{
						{Name: configVolume, VolumeSource: configSource},
					},
				},
			},
		},
	}
	*srv.OwnerReferences[0].Controller = true
	*deploy.OwnerReferences[0].Controller = true

	// Create the deployment and touch anything that needs to know its name.
	if err := r.Client.Create(ctx, &deploy); err != nil {
		return err
	}
	if err := next.Status.Deployment.From(&deploy); err != nil {
		return err
	}
	cfgAnno[clairv1alpha1.TemplateIndexerDeployment] = deploy.Namespace + "/" + deploy.Name
	log.Info("created deployment", "ref", deploy.Namespace+"/"+deploy.Name)

	// Create the service and anything that needs to know its name.
	if err := r.Client.Create(ctx, &srv); err != nil {
		return err
	}
	if err := next.Status.Service.From(&srv); err != nil {
		return err
	}
	cfgAnno[clairv1alpha1.TemplateIndexerService] = srv.Namespace + "/" + srv.Name
	log.Info("created service", "ref", srv.Namespace+"/"+srv.Name)

	if r.options.HPA {
		// Create the HPA.
		hpa := scalev2.HorizontalPodAutoscaler{
			ObjectMeta: mkMeta(serviceName, &cur.ObjectMeta),
			Spec: scalev2.HorizontalPodAutoscalerSpec{
				ScaleTargetRef: scalev2.CrossVersionObjectReference{
					Kind:       "Deployment",
					APIVersion: "apps/v1",
					Name:       deploy.Name,
				},
				MaxReplicas: 10, // wild guess
				Metrics:     nil,
				// TODO(hank) Set up custom HPA metrics to scale on. This may
				// require some ranging to see if custom metrics are enabled and
				// how.
			},
		}
		*hpa.OwnerReferences[0].Controller = true
		if err := r.Client.Create(ctx, &hpa); err != nil {
			return err
		}
		if err := next.Status.Autoscaler.From(&hpa); err != nil {
			return err
		}
		log.Info("created hpa", "ref", hpa.Namespace+"/"+hpa.Name)
	}

	if r.options.Monitor {
		// Create our metrics monitor.
		monitor := monitorv1.ServiceMonitor{
			ObjectMeta: mkMeta(serviceName, &cur.ObjectMeta),
			Spec: monitorv1.ServiceMonitorSpec{
				Endpoints: []monitorv1.Endpoint{
					{
						Port:     clairv1alpha1.PortIntrospection,
						Scheme:   `http`,
						Interval: (30 * time.Second).String(),
					},
				},
				Selector: metav1.LabelSelector{MatchLabels: selectors},
			},
		}
		*monitor.OwnerReferences[0].Controller = true
		monitor.Labels[`k8s-app`] = cur.Name // This seems to be the standard? It's hard to tell.
		if err := r.Client.Create(ctx, &monitor); err != nil {
			return err
		}
		log.Info("created servicemonitor", "ref", monitor.Namespace+"/"+monitor.Name)
	}

	// Purposefully grab the current version number?
	//
	// Don't know if we'll see an update from the annotation changes.
	next.Status.ConfigVersion = cfg.GetResourceVersion()
	// Add a non-controlling owner ref so that we get notifications when things
	// change.
	cfg.SetOwnerReferences(append(cfg.GetOwnerReferences(), metav1.OwnerReference{Name: cur.Name}))
	if err := r.Client.Update(ctx, cfg); err != nil {
		return err
	}
	log.Info("config updated")

	return nil
}

// SetupWithManager sets up the controller with the Manager.
func (r *IndexerReconciler) SetupWithManager(mgr ctrl.Manager) error {
	apiType := &clairv1alpha1.Indexer{}
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
	mapper := mgr.GetRESTMapper()
	for _, obj := range []client.Object{
		&scalev2.HorizontalPodAutoscaler{},
		&monitorv1.ServiceMonitor{},
	} {
		gvk := obj.GetObjectKind().GroupVersionKind()
		_, err := mapper.RESTMapping(gvk.GroupKind(), gvk.Version)
		if err != nil {
			r.Log.Error(err, "missing optionally supported resource", "gvk", gvk.String())
			continue
		}
		b = b.Owns(obj)
		r.Log.Info("found optional kind", "gvk", gvk.String())
		r.options.Set(gvk)
	}
	return b.Complete(r)
}
