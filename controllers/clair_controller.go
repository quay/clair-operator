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
	"fmt"
	"strings"

	"github.com/go-logr/logr"
	routev1 "github.com/openshift/api/route/v1"
	monitorv1 "github.com/prometheus-operator/prometheus-operator/pkg/apis/monitoring/v1"
	appsv1 "k8s.io/api/apps/v1"
	scalev2 "k8s.io/api/autoscaling/v2beta2"
	batchv1 "k8s.io/api/batch/v1"
	corev1 "k8s.io/api/core/v1"
	netv1 "k8s.io/api/networking/v1"
	k8serr "k8s.io/apimachinery/pkg/api/errors"
	metav1 "k8s.io/apimachinery/pkg/apis/meta/v1"
	"k8s.io/apimachinery/pkg/runtime"
	"k8s.io/apimachinery/pkg/runtime/schema"
	"k8s.io/client-go/discovery"
	ctrl "sigs.k8s.io/controller-runtime"
	"sigs.k8s.io/controller-runtime/pkg/client"
	"sigs.k8s.io/controller-runtime/pkg/controller/controllerutil"

	clairv1alpha1 "github.com/quay/clair-operator/api/v1alpha1"
)

func Key(s string) string {
	const prefix = `clair.projectquay.io/`
	return prefix + strings.Map(func(r rune) rune {
		switch r {
		case '_', ' ', '\t', '\n':
			return '-'
		}
		return r
	}, strings.ToLower(s))
}

// ClairReconciler reconciles a Clair object
type ClairReconciler struct {
	client.Client
	Log     logr.Logger
	Scheme  *runtime.Scheme
	options optionalTypes
}

// +kubebuilder:rbac:groups=clair.projectquay.io,resources=clairs,verbs=get;list;watch;create;update;patch;delete
// +kubebuilder:rbac:groups=clair.projectquay.io,resources=clairs/status,verbs=get;update;patch
// +kubebuilder:rbac:groups=clair.projectquay.io,resources=clairs/finalizers,verbs=update
// +kubebuilder:rbac:groups=apps,resources=deployments,verbs=get;list;watch;create;update;patch;delete
// +kubebuilder:rbac:groups=core,resources=pods,verbs=get;list
// +kubebuilder:rbac:groups=core,resources=service,verbs=get;list;watch;create;update;patch;delete
// +kubebuilder:rbac:groups=core,resources=secret,verbs=get;list;watch;create;update;patch;delete
// +kubebuilder:rbac:groups=core,resources=configmap,verbs=get;list;watch;create;update;patch;delete
// +kubebuilder:rbac:groups=networking.k8s.io,resources=ingress,verbs=get;list;watch;create;update;patch;delete

// Reconcile is part of the main kubernetes reconciliation loop which aims to
// move the current state of the cluster closer to the desired state.
// TODO(user): Modify the Reconcile function to compare the state specified by
// the Clair object against the actual cluster state, and then
// perform operations to make the cluster state reflect the state specified by
// the user.
//
// For more details, check Reconcile and its Result here:
// - https://pkg.go.dev/sigs.k8s.io/controller-runtime@v0.7.0/pkg/reconcile
func (r *ClairReconciler) Reconcile(ctx context.Context, req ctrl.Request) (ctrl.Result, error) {
	log := r.Log.WithValues("clair", req.NamespacedName)
	log.Info("begin reconcile")
	defer log.Info("end reconcile")

	var (
		cur clairv1alpha1.Clair
		res ctrl.Result
	)
	err := r.Get(ctx, req.NamespacedName, &cur)
	switch {
	case err == nil:
	case k8serr.IsNotFound(err):
		// ???
		return res, nil
	default:
		return res, client.IgnoreNotFound(err)
	}
	next := cur.DeepCopy()

	if cur.Status.Config == nil {
		return r.initialize(ctx, &cur, next)
	}

	// Check databases:
	managedDB := cur.Spec.Databases == nil
	createdDB := cur.Status.Database != nil
	switch {
	case managedDB && createdDB, !managedDB && !createdDB:
		// OK
	case managedDB && !createdDB:
		// make our database
		res.Requeue = true
		return res, nil
	case !managedDB && createdDB:
		// Tear down our managed database, because the spec has changed to
		// indicate that everything will be using an unmanaged database.
		db := cur.Status.Database

		deploy := appsv1.Deployment{ObjectMeta: metav1.ObjectMeta{
			Namespace: cur.Namespace,
			Name:      db.Deployment.Name,
		}}
		if err := r.Delete(ctx, &deploy); err != nil && !k8serr.IsNotFound(err) {
			return res, err
		}

		service := corev1.Service{ObjectMeta: metav1.ObjectMeta{
			Namespace: cur.Namespace,
			Name:      db.Service.Name,
		}}
		if err = r.Delete(ctx, &service); err != nil && !k8serr.IsNotFound(err) {
			return res, err
		}

		next.Status.Database = nil
		if err := r.Update(ctx, next); err != nil {
			return res, err
		}
		res.Requeue = true
		return res, nil
	}

	return res, nil
}

func (r *ClairReconciler) initialize(ctx context.Context, cur, next *clairv1alpha1.Clair) (ctrl.Result, error) {
	log := r.Log
	var res ctrl.Result

	status := next.Status
	if !cur.Status.Indexer.Populated() {
		switch {
		case cur.Status.Indexer == nil:
			next.Status.Indexer = &clairv1alpha1.ServiceRef{}
			fallthrough
		case cur.Status.Indexer.Service == nil:
			srv := corev1.Service{
				ObjectMeta: metav1.ObjectMeta{
					GenerateName: "clair-indexer",
					Namespace:    cur.GetNamespace(),
					Labels: map[string]string{
						"clair.projectquay.io/owner": string(cur.UID),
					},
				},
				Spec: corev1.ServiceSpec{
					Selector: map[string]string{
						"clair.projectquay.io/service-indexer": "true",
						"clair.projectquay.io/owner":           string(cur.UID),
					},
				},
			}
			if err := r.Create(ctx, &srv); err != nil {
				return res, err
			}
			if err := r.Update(ctx, next); err != nil {
				return res, err
			}
			next.DeepCopyInto(cur)
		case cur.Status.Indexer.Deployment == nil:
		}
	}

	managedDB := cur.Spec.Databases == nil
	if managedDB {
		k, err := newKustomize()
		if err != nil {
			return res, err
		}
		rm, err := k.Database(postgresImage)
		if err != nil {
			return res, err
		}
		// These are all core APIs. If something ends up unpopulated, there are
		// bigger issues afoot.
		var (
			sset          appsv1.StatefulSet
			job           batchv1.Job
			srv           corev1.Service
			iDB, mDB, nDB corev1.Secret
			// TODO(hank) Certmanager madness?
		)
		for _, tmpl := range rm.Resources() {
			tmpl.SetNamespace(cur.GetNamespace())
			b, err := tmpl.MarshalJSON()
			if err != nil {
				return res, err
			}
			log.Info("resource", "res", tmpl.GetKind()+"/"+tmpl.GetName())
			switch k := tmpl.GetKind(); k {
			case "StatefulSet":
				if err := json.Unmarshal(b, &sset); err != nil {
					return res, err
				}
				if err := controllerutil.SetControllerReference(cur, &sset, r.Scheme); err != nil {
					return res, err
				}
			case "Job":
				if err := json.Unmarshal(b, &job); err != nil {
					return res, err
				}
				if err := controllerutil.SetControllerReference(cur, &job, r.Scheme); err != nil {
					return res, err
				}
			case "Service":
				if err := json.Unmarshal(b, &srv); err != nil {
					return res, err
				}
				if err := controllerutil.SetControllerReference(cur, &srv, r.Scheme); err != nil {
					return res, err
				}
			case "Secret":
				var sec *corev1.Secret
				switch n := tmpl.GetName(); n {
				case "notifier-db":
					sec = &nDB
				case "indexer-db":
					sec = &iDB
				case "matcher-db":
					sec = &mDB
				default:
					log.Info("unknown secret", "name", n)
				}
				if err := json.Unmarshal(b, sec); err != nil {
					return res, err
				}
				if err := controllerutil.SetControllerReference(cur, sec, r.Scheme); err != nil {
					return res, err
				}
			default:
				log.Info("unknown resource", "kind", k)
			}
		}

		for _, obj := range []client.Object{
			&sset, &job, &srv, &nDB, &iDB, &mDB,
		} {
			if err := r.Create(ctx, obj); err != nil {
				return res, err
			}
			if err := status.AddRef(obj, r.Scheme); err != nil {
				return res, err
			}
		}

	}

	return res, nil
}

func makeDNS(obj metav1.Object, srv *corev1.Service) string {
	return fmt.Sprintf(`%s.%s.svc.%s`, srv.Name, srv.Namespace, obj.GetClusterName())
}

var wantGVKs = map[string]map[string]struct{}{
	routev1.SchemeGroupVersion.String():   {"Route": {}},
	monitorv1.SchemeGroupVersion.String(): {"ServiceMonitor": {}},
	scalev2.SchemeGroupVersion.String():   {"HorizontalPodAutoScaler": {}},
	netv1.SchemeGroupVersion.String():     {"Ingress": {}},
}

// SetupWithManager sets up the controller with the Manager.
func (r *ClairReconciler) SetupWithManager(mgr ctrl.Manager) error {
	b := ctrl.NewControllerManagedBy(mgr).
		For(&clairv1alpha1.Clair{}).
		Owns(&appsv1.Deployment{}).
		Owns(&corev1.Service{}).
		Owns(&corev1.Secret{}).
		Owns(&corev1.ConfigMap{})

	// TODO(hank) Maybe do this at creation time instead of startup?
	dc, err := discovery.NewDiscoveryClientForConfig(mgr.GetConfig())
	if err != nil {
		return err
	}
	for gv, rs := range wantGVKs {
		rl, err := dc.ServerResourcesForGroupVersion(gv)
		if err != nil {
			return err
		}
		for _, ar := range rl.APIResources {
			if _, ok := rs[ar.Kind]; ok {
				gvk := schema.GroupVersionKind{
					Group:   ar.Group,
					Version: ar.Version,
					Kind:    ar.Kind,
				}
				r.Log.Info("found optional kind", "gvk", gvk.String())
				r.options.Set(gvk)
			}
		}
	}

	return b.Complete(r)
}
