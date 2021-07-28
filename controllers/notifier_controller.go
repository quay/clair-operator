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

	k8serr "k8s.io/apimachinery/pkg/api/errors"
	metav1 "k8s.io/apimachinery/pkg/apis/meta/v1"
	ctrl "sigs.k8s.io/controller-runtime"
	"sigs.k8s.io/controller-runtime/pkg/client"
	logf "sigs.k8s.io/controller-runtime/pkg/log"

	clairv1alpha1 "github.com/quay/clair-operator/api/v1alpha1"
)

// NotifierReconciler reconciles a Notifier object
type NotifierReconciler struct {
	ServiceReconciler
}

// +kubebuilder:rbac:groups=clair.projectquay.io,resources=notifiers,verbs=get;list;watch;create;update;patch;delete
// +kubebuilder:rbac:groups=clair.projectquay.io,resources=notifiers/status,verbs=get;update;patch
// +kubebuilder:rbac:groups=clair.projectquay.io,resources=notifiers/finalizers,verbs=update
// +kubebuilder:rbac:groups=apps,resources=deployments,verbs=get;list;watch;create;update;patch;delete
// +kubebuilder:rbac:groups=core,resources=pods,verbs=get;list
// +kubebuilder:rbac:groups=core,resources=service,verbs=get;list;watch;create;update;patch;delete
// +kubebuilder:rbac:groups=core,resources=secret,verbs=get;list;watch;create;update;patch;delete
// +kubebuilder:rbac:groups=core,resources=configmap,verbs=get;list;watch;create;update;patch;delete

// Reconcile is part of the main kubernetes reconciliation loop which aims to
// move the current state of the cluster closer to the desired state.
// TODO(user): Modify the Reconcile function to compare the state specified by
// the Notifier object against the actual cluster state, and then
// perform operations to make the cluster state reflect the state specified by
// the user.
//
// For more details, check Reconcile and its Result here:
// - https://pkg.go.dev/sigs.k8s.io/controller-runtime@v0.7.0/pkg/reconcile
func (r *NotifierReconciler) Reconcile(ctx context.Context, req ctrl.Request) (ctrl.Result, error) {
	log := r.Log.WithValues("notifier", req.NamespacedName)
	ctx = logf.IntoContext(ctx, log)
	log.Info("start")
	defer log.Info("done")
	var (
		cur clairv1alpha1.Notifier
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
		return r.InflateTemplates(ctx, &cur, cur.DeepCopy(), cfg)
	case configChanged && !emptyRefs:
		log.Info("need to check resources")
		return r.CheckResources(ctx, &cur, cur.DeepCopy(), cfg)
	case !configChanged && !emptyRefs:
		log.Info("unsure why the controller was notified")
		return res, nil
	}
	return res, nil
}

// SetupWithManager sets up the controller with the Manager.
func (r *NotifierReconciler) SetupWithManager(mgr ctrl.Manager) error {
	r.Log = mgr.GetLogger().WithName("Notifier")
	b, err := r.SetupService(mgr, &clairv1alpha1.Notifier{})
	if err != nil {
		return err
	}
	return b.Complete(r)
}
