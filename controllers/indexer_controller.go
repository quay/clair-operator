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
	"time"

	appsv1 "k8s.io/api/apps/v1"
	scalev2 "k8s.io/api/autoscaling/v2beta2"
	corev1 "k8s.io/api/core/v1"
	k8serr "k8s.io/apimachinery/pkg/api/errors"
	metav1 "k8s.io/apimachinery/pkg/apis/meta/v1"
	"k8s.io/apimachinery/pkg/types"
	ctrl "sigs.k8s.io/controller-runtime"
	"sigs.k8s.io/controller-runtime/pkg/client"
	logf "sigs.k8s.io/controller-runtime/pkg/log"

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
		cfg, err := r.config(ctx, cur.Namespace, cur.Spec.Config)
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
	//now       = time.Now()
	//selectors = map[string]string{ServiceSelectorKey: serviceName, GroupSelectorKey: cur.Name}
	)
	log := logf.FromContext(ctx)

	// Populate the config source for our container volume.
	cfg, err := r.config(ctx, cur.Namespace, cur.Spec.Config)
	if err != nil {
		return err
	}
	log.Info("found config", "ref", cur.Spec.Config)
	cfgAnno := cfg.GetAnnotations()

	k := newKustomize()
	res, err := k.Indexer(cfg)
	if err != nil {
		return err
	}
	var (
		deploy appsv1.Deployment
		srv    corev1.Service
		hpa    scalev2.HorizontalPodAutoscaler
	)
	for _, r := range res.Resources() {
		b, err := r.MarshalJSON()
		if err != nil {
			return err
		}
		switch r.GetKind() {
		case "Deployment":
			if err := json.Unmarshal(b, &deploy); err != nil {
				return err
			}
			deploy.OwnerReferences = []metav1.OwnerReference{
				metav1.OwnerReference{
					APIVersion: cur.APIVersion,
					Kind:       cur.Kind,
					Name:       cur.Name,
					UID:        cur.UID,
				},
			}
			*deploy.OwnerReferences[0].Controller = true
			name := deploy.Namespace + "/" + deploy.Name
			cfgAnno[clairv1alpha1.TemplateIndexerDeployment] = name
			log.Info("new deployment", "ref", name)
		case "Service":
			if err := json.Unmarshal(b, &srv); err != nil {
				return err
			}
			srv.OwnerReferences = []metav1.OwnerReference{
				metav1.OwnerReference{
					APIVersion: cur.APIVersion,
					Kind:       cur.Kind,
					Name:       cur.Name,
					UID:        cur.UID,
				},
			}
			*srv.OwnerReferences[0].Controller = true
			name := srv.Namespace + "/" + srv.Name
			cfgAnno[clairv1alpha1.TemplateIndexerService] = name
			log.Info("new service", "ref", name)
		case "HorizontalPodAutoscaler":
			if err := json.Unmarshal(b, &hpa); err != nil {
				return err
			}
			hpa.OwnerReferences = []metav1.OwnerReference{
				metav1.OwnerReference{
					APIVersion: cur.APIVersion,
					Kind:       cur.Kind,
					Name:       cur.Name,
					UID:        cur.UID,
				},
			}
			*hpa.OwnerReferences[0].Controller = true
			name := hpa.Namespace + "/" + hpa.Name
			log.Info("new hpa", "ref", name)
		default:
			log.Info("unknown resource", "kind", r.GetKind())
		}
	}

	// Create the deployment and touch anything that needs to know its name.
	if err := r.Client.Create(ctx, &deploy); err != nil {
		return err
	}
	if err := next.Status.Deployment.From(&deploy); err != nil {
		return err
	}
	log.Info("created deployment", "ref", cfgAnno[clairv1alpha1.TemplateIndexerDeployment])

	// Create the service and anything that needs to know its name.
	if err := r.Client.Create(ctx, &srv); err != nil {
		return err
	}
	if err := next.Status.Service.From(&srv); err != nil {
		return err
	}
	cfgAnno[clairv1alpha1.TemplateIndexerService] = srv.Namespace + "/" + srv.Name
	log.Info("created service", "ref", srv.Namespace+"/"+srv.Name)

	/*
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
	*/

	// Purposefully grab the current version number?
	//
	// Don't know if we'll see an update from the annotation changes.
	next.Status.ConfigVersion = cfg.GetResourceVersion()
	// Add a non-controlling owner ref so that we get notifications when things
	// change.
	cfg.SetOwnerReferences(append(cfg.GetOwnerReferences(), metav1.OwnerReference{
		UID: cur.UID,
	}))
	if err := r.Client.Update(ctx, cfg); err != nil {
		return err
	}
	log.Info("config updated")

	return nil
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
