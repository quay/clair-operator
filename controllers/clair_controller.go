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
	"strings"

	"github.com/go-logr/logr"
	routev1 "github.com/openshift/api/route/v1"
	appsv1 "k8s.io/api/apps/v1"
	corev1 "k8s.io/api/core/v1"
	k8serr "k8s.io/apimachinery/pkg/api/errors"
	metav1 "k8s.io/apimachinery/pkg/apis/meta/v1"
	"k8s.io/apimachinery/pkg/runtime"
	"k8s.io/apimachinery/pkg/runtime/schema"
	ctrl "sigs.k8s.io/controller-runtime"
	"sigs.k8s.io/controller-runtime/pkg/client"

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
	Log    logr.Logger
	Scheme *runtime.Scheme
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
		if err := r.Client.Delete(ctx, &deploy); err != nil && !k8serr.IsNotFound(err) {
			return res, err
		}

		service := corev1.Service{ObjectMeta: metav1.ObjectMeta{
			Namespace: cur.Namespace,
			Name:      db.Service.Name,
		}}
		if err = r.Client.Delete(ctx, &service); err != nil && !k8serr.IsNotFound(err) {
			return res, err
		}

		next.Status.Database = nil
		if err := r.Client.Update(ctx, next); err != nil {
			return res, err
		}
		res.Requeue = true
		return res, nil
	}

	return res, nil
}

func (r *ClairReconciler) initialize(ctx context.Context, cur, next *clairv1alpha1.Clair) (ctrl.Result, error) {
	var res ctrl.Result
	if err := r.rangefind(ctx, cur, next); err != nil {
		return res, err
	}

	managedDB := cur.Spec.Databases == nil
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
			if err := r.Client.Create(ctx, &srv); err != nil {
				return res, err
			}
			if err := r.Client.Update(ctx, next); err != nil {
				return res, err
			}
			next.DeepCopyInto(cur)
		case cur.Status.Indexer.Deployment == nil:
		}
	}
	_ = managedDB

	return res, nil
}

func makeDNS(obj metav1.Object, srv *corev1.Service) string {
	return fmt.Sprintf(`%s.%s.svc.%s`, srv.Name, srv.Namespace, obj.GetClusterName())
}

const (
	routeAnnotation = `clair.projectquay.io/has-route`
	aTrue           = string(metav1.ConditionTrue)
	aFalse          = string(metav1.ConditionFalse)
)

var (
	gvkMap = map[string]schema.GroupVersionKind{
		routeAnnotation: (&routev1.Route{}).GroupVersionKind(),
	}
	needAnnotations = []struct {
		Group       string
		Annotations []string
	}{
		{"routing", []string{routeAnnotation}},
	}
)

type availableKinds struct {
	Routing schema.GroupVersionKind
}

func (ak *availableKinds) check(as map[string]string) error {
	// See what we're missing, if anything, and figure out what to use.
CheckGroup:
	for _, s := range needAnnotations {
		for _, k := range s.Annotations {
			if val, ok := as[k]; ok && val == aTrue {
				switch s.Group {
				case "routing":
					ak.Routing = gvkMap[k]
				}
				continue CheckGroup
			}
		}
	}
	return nil
}

// Rangefind tests for all the capabilities we'd like to make use of and adds
// annotations to the resource.
//
// If any updates are made, the objects pointed to by cur and next will be
// updated accordingly.
func (r *ClairReconciler) rangefind(ctx context.Context, cur, next *clairv1alpha1.Clair) error {
	var missing []string
	changed := false
	mapper := r.Client.RESTMapper()
	as := cur.GetAnnotations()

	// First, check for the existence of all of the resources we could use.
	for key, gvk := range gvkMap {
		if _, ok := as[key]; ok {
			continue
		}
		_, err := mapper.RESTMapping(gvk.GroupKind(), gvk.Version)
		if err != nil {
			as[key] = aFalse
		} else {
			as[key] = aTrue
		}
		changed = true
	}

	// See what we're missing, if anything, and figure out what to use.
CheckGroup:
	for _, s := range needAnnotations {
		for _, k := range s.Annotations {
			if val, ok := as[k]; ok && val == aTrue {
				continue CheckGroup
			}
		}
		missing = append(missing, s.Group)
	}

	if changed {
		// Update our annotations.
		next.SetAnnotations(as)
		// Set the condition appropriately.
		if missing != nil {
			next.SetCondition(clairv1alpha1.ClairConfigBlocked, metav1.ConditionTrue,
				clairv1alpha1.ClairReasonMissingDeps, fmt.Sprintf("missing needed types: %v", missing))
		} else {
			next.SetCondition(clairv1alpha1.ClairConfigBlocked, metav1.ConditionFalse, "", "")
		}
		if err := r.Client.Update(ctx, next); err != nil {
			return err
		}
		next.DeepCopyInto(cur)
	}

	if missing != nil {
		return fmt.Errorf("missing needed types: %v", missing)
	}
	return nil
}

// SetupWithManager sets up the controller with the Manager.
func (r *ClairReconciler) SetupWithManager(mgr ctrl.Manager) error {
	return ctrl.NewControllerManagedBy(mgr).
		For(&clairv1alpha1.Clair{}).
		Owns(&appsv1.Deployment{}).
		Owns(&corev1.Service{}).
		Owns(&corev1.Secret{}).
		Owns(&corev1.ConfigMap{}).
		Complete(r)
}