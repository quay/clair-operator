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
	"encoding/json"

	appsv1 "k8s.io/api/apps/v1"
	scalev2 "k8s.io/api/autoscaling/v2beta2"
	corev1 "k8s.io/api/core/v1"
	k8serr "k8s.io/apimachinery/pkg/api/errors"
	metav1 "k8s.io/apimachinery/pkg/apis/meta/v1"
	"k8s.io/apimachinery/pkg/apis/meta/v1/unstructured"
	ctrl "sigs.k8s.io/controller-runtime"
	"sigs.k8s.io/controller-runtime/pkg/client"
	"sigs.k8s.io/controller-runtime/pkg/controller/controllerutil"
	logf "sigs.k8s.io/controller-runtime/pkg/log"

	monitorv1 "github.com/prometheus-operator/prometheus-operator/pkg/apis/monitoring/v1"

	clairv1alpha1 "github.com/quay/clair-operator/api/v1alpha1"
)

// IndexerReconciler reconciles a Indexer object
type IndexerReconciler struct {
	ServiceReconciler
}

/*
The basic logic for the Indexer reconciler is:

1. Load the state of the world.
2. Create any missing Objects.
3. Check annotations.
4. Restart anything needed.
*/

func indexerState(cs []metav1.Condition) (string, error) {
	var states = []string{
		`Empty`,
		`clair.projectquay.io/ServiceCreated`,
		`clair.projectquay.io/DeploymentCreated`,
		`clair.projectquay.io/Steady`,
		`clair.projectquay.io/Redeploying`,
	}
	m := conditionMap(cs, states[1:])
	for i, s := range states[1:3] {
		// For these states, if not True (so, False or Unknown), return the
		// previous state. Note the reslicing.
		if m[s] != metav1.ConditionTrue {
			return states[i], nil
		}
	}
	steady, redeploy := m[states[3]] == metav1.ConditionTrue, m[states[4]] == metav1.ConditionTrue
	switch {
	case !steady, !redeploy:
		// In a failure state
	case !steady, redeploy:
		// Redeploying, check
	case steady, !redeploy:
		// Steady
	case steady, redeploy:
		// redeploy check
	}
	return "", nil
}

// +kubebuilder:rbac:groups=clair.projectquay.io,resources=indexers,verbs=get;list;watch;create;update;patch;delete
// +kubebuilder:rbac:groups=clair.projectquay.io,resources=indexers/status,verbs=get;update;patch
// +kubebuilder:rbac:groups=clair.projectquay.io,resources=indexers/finalizers,verbs=update
// +kubebuilder:rbac:groups=apps,resources=deployments,verbs=get;list;watch;create;update;patch;delete
// +kubebuilder:rbac:groups=core,resources=pods,verbs=get;list
// +kubebuilder:rbac:groups=core,resources=service,verbs=get;list;watch;create;update;patch;delete
// +kubebuilder:rbac:groups=core,resources=secret,verbs=get;list;watch;create;update;patch;delete
// +kubebuilder:rbac:groups=core,resources=configmap,verbs=get;list;watch;create;update;patch;delete

// Reconcile is part of the main kubernetes reconciliation loop which aims to
// move the current state of the cluster closer to the desired state.
func (r *IndexerReconciler) Reconcile(ctx context.Context, req ctrl.Request) (ctrl.Result, error) {
	log := r.Log.WithValues("indexer", req.NamespacedName)
	ctx = logf.IntoContext(ctx, log)
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

	// If our spec isn't complete, post a note and then chill.
	if cur.Spec.Config == nil {
		next := cur.DeepCopy()
		next.Status.Conditions = append(next.Status.Conditions, metav1.Condition{
			Type:               clairv1alpha1.ServiceAvailable,
			ObservedGeneration: cur.Generation,
			LastTransitionTime: metav1.Now(),
			Status:             metav1.ConditionFalse,
			Reason:             "InvalidSpec",
			Message:            `spec missing "config"`,
		})

		if err := r.Client.Status().Update(ctx, next); err != nil {
			return res, err
		}
		return res, nil
	}

	cfg, err := r.config(ctx, cur.Namespace, cur.Spec.Config)
	if err != nil {
		return res, err
	}
	configChanged := cfg.GetResourceVersion() != cur.Status.ConfigVersion
	emptyRefs := len(cur.Status.Refs) == 0
	switch {
	case configChanged && emptyRefs:
		log.Info("initial run")
		fallthrough
	case !configChanged && emptyRefs:
		log.Info("inflating templates")
		return r.indexerTemplates(ctx, &cur, cfg)
	case configChanged && !emptyRefs:
		log.Info("need to check resources")
		return r.checkResources(ctx, &cur, cfg)
	case !configChanged && !emptyRefs:
		log.Info("unsure why the controller was notified")
		return res, nil
	}
	return res, nil
}

func (r *IndexerReconciler) indexerTemplates(ctx context.Context, cur *clairv1alpha1.Indexer, cfg *unstructured.Unstructured) (ctrl.Result, error) {
	const (
		// TODO(hank) Allow configuration, by environment variable?
		img = `quay.io/projectquay/clair:4.0.0`
	)
	log := logf.FromContext(ctx)
	next := cur.DeepCopy()

	cfgAnno := cfg.GetAnnotations()
	if cfgAnno == nil {
		cfgAnno = make(map[string]string)
	}

	res, err := r.k.Indexer(cfg)
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
		tmpl.SetNamespace(cur.Namespace)
		b, err := tmpl.MarshalJSON()
		if err != nil {
			return ctrl.Result{}, err
		}
		log.Info("resource", "res", tmpl.GetKind())
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
	if err := next.Status.AddRef(&deploy, r.Scheme); err != nil {
		return ctrl.Result{}, err
	}
	if err := r.Client.Status().Update(ctx, next); err != nil {
		return ctrl.Result{}, err
	}
	cfgAnno[clairv1alpha1.TemplateIndexerDeployment] = deploy.Namespace + "/" + deploy.Name
	log.Info("created deployment", "ref", cfgAnno[clairv1alpha1.TemplateIndexerDeployment])

	// Create the service and anything that needs to know its name.
	if err := r.Client.Create(ctx, &srv); err != nil {
		return ctrl.Result{}, err
	}
	if err := next.Status.AddRef(&srv, r.Scheme); err != nil {
		return ctrl.Result{}, err
	}
	if err := r.Client.Status().Update(ctx, next); err != nil {
		return ctrl.Result{}, err
	}
	cfgAnno[clairv1alpha1.TemplateIndexerService] = srv.Namespace + "/" + srv.Name
	log.Info("created service", "ref", srv.Namespace+"/"+srv.Name)

	if r.options.HPA {
		if err := r.Client.Create(ctx, &hpa); err != nil {
			return ctrl.Result{}, err
		}
		if err := next.Status.AddRef(&hpa, r.Scheme); err != nil {
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
		if err := next.Status.AddRef(&monitor, r.Scheme); err != nil {
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
	next.Status.ConfigVersion = cfg.GetResourceVersion()
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

func (r *IndexerReconciler) checkResources(ctx context.Context, cur *clairv1alpha1.Indexer, cfg *unstructured.Unstructured) (ctrl.Result, error) {
	log := logf.FromContext(ctx)
	log.Info("checking resources")
	// Check annotations
	a := cfg.GetAnnotations()
	if a == nil {
		a = make(map[string]string)
	}
	var (
		deployName string
		srvName    string
		changed    bool
	)
	for _, r := range cur.Status.Refs {
		switch r.Kind {
		case "Deployment":
			deployName = cur.Namespace + "/" + r.Name
		case "Service":
			srvName = cur.Namespace + "/" + r.Name
		}
	}
	switch {
	case deployName == "":
		log.Info("missing deployment")
	case a[clairv1alpha1.TemplateIndexerDeployment] != deployName:
		log.Info("updating configuration deployment")
		changed = true
		a[clairv1alpha1.TemplateIndexerDeployment] = deployName
	}
	switch {
	case srvName == "":
		log.Info("missing service")
	case a[clairv1alpha1.TemplateIndexerService] != srvName:
		log.Info("updating configuration service")
		changed = true
		a[clairv1alpha1.TemplateIndexerService] = srvName
	}
	if changed {
		cfg.SetAnnotations(a)
		if err := r.Client.Update(ctx, cfg); err != nil {
			return ctrl.Result{}, err
		}
		log.Info("updated config", "name", cfg.GetName())
		return ctrl.Result{Requeue: true}, nil
	}

	var curStatus *metav1.Condition
	for _, s := range cur.Status.Conditions {
		if s.Type == clairv1alpha1.ServiceAvailable {
			curStatus = &s
			break
		}
	}
	// Check deployment Status
	log.Info("checking refs")
	cnd, err := r.CheckRefsAvailable(ctx, cur, cur.Status.Refs)
	if err != nil {
		return ctrl.Result{}, err
	}
	switch {
	case curStatus == nil:
		next := cur.DeepCopy()
		next.Status.Conditions = append(next.Status.Conditions, cnd)
		if err := r.Client.Status().Update(ctx, next); err != nil {
			return ctrl.Result{}, err
		}
	case curStatus.Reason != cnd.Reason:
		log.V(1).Info("updating: dependent resources changed", "condition", cnd)
		next := cur.DeepCopy()
		for i, sc := range next.Status.Conditions {
			if sc.Type == cnd.Type {
				cnd.DeepCopyInto(&next.Status.Conditions[i])
				break
			}
		}
		if err := r.Client.Status().Update(ctx, next); err != nil {
			return ctrl.Result{}, err
		}
	case curStatus.Reason == cnd.Reason:
		log.V(1).Info("skipping update: dependent resources unchanged")
	}

	return ctrl.Result{}, nil
}

// SetupWithManager sets up the controller with the Manager.
func (r *IndexerReconciler) SetupWithManager(mgr ctrl.Manager) error {
	r.Log = mgr.GetLogger().WithName("Indexer")
	b, err := r.SetupService(mgr, &clairv1alpha1.Indexer{})
	if err != nil {
		return err
	}
	return b.Complete(r)
}
