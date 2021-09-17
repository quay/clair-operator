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
	"path/filepath"
	"reflect"
	"testing"
	"time"

	"github.com/go-logr/zapr"
	"go.uber.org/zap/zapcore"
	"go.uber.org/zap/zaptest"
	appsv1 "k8s.io/api/apps/v1"
	scalev2 "k8s.io/api/autoscaling/v2beta2"
	corev1 "k8s.io/api/core/v1"
	metav1 "k8s.io/apimachinery/pkg/apis/meta/v1"
	"k8s.io/apimachinery/pkg/runtime"
	"k8s.io/apimachinery/pkg/types"
	ctrl "sigs.k8s.io/controller-runtime"
	"sigs.k8s.io/controller-runtime/pkg/client"
	"sigs.k8s.io/controller-runtime/pkg/envtest"
	"sigs.k8s.io/controller-runtime/pkg/log"

	clairv1alpha1 "github.com/quay/clair-operator/api/v1alpha1"
	// +kubebuilder:scaffold:imports
)

// EnvSetup starts an envtest instance and pre-populates it with the local
// types.
//
// This function is largely ported from the one spit out by the scaffolding,
// with all the ginkgo/gomega pulled out.
func EnvSetup(ctx context.Context, t testing.TB) (context.Context, client.Client) {
	logger := zapr.NewLogger(zaptest.NewLogger(t, zaptest.Level(zapcore.DebugLevel))).
		WithName("controllers")
	ctx = log.IntoContext(ctx, logger)

	ctx, done := context.WithCancel(ctx)
	t.Cleanup(done)

	env := envtest.Environment{
		CRDDirectoryPaths: []string{filepath.Join("..", "config", "crd", "bases")},
	}
	cfg, err := env.Start()
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() {
		if err := env.Stop(); err != nil {
			t.Log(err)
		}
	})
	t.Log("environment started")

	scheme := runtime.NewScheme()
	for _, f := range []func(*runtime.Scheme) error{
		clairv1alpha1.AddToScheme,
		corev1.AddToScheme,
		appsv1.AddToScheme,
		scalev2.AddToScheme,
		// Add more groups here as needed.
	} {
		if err := f(scheme); err != nil {
			t.Fatal(err)
		}
	}
	t.Log("schemes registered")

	// +kubebuilder:scaffold:scheme

	mgr, err := ctrl.NewManager(cfg, ctrl.Options{
		Scheme: scheme,
		Logger: logger,
	})
	if err != nil {
		t.Fatal(err)
	}
	if err := (&IndexerReconciler{}).SetupWithManager(mgr); err != nil {
		t.Fatal(err)
	}
	if err := (&MatcherReconciler{}).SetupWithManager(mgr); err != nil {
		t.Fatal(err)
	}
	mgrctx, mgrdone := context.WithCancel(ctx)
	go func() {
		if err := mgr.Start(mgrctx); err != nil {
			t.Errorf("error from Manager.Start: %v", err)
		}
	}()
	t.Cleanup(mgrdone)

	return ctx, mgr.GetClient()
}

// ServiceTestcase is a helper struct for testing the service reconcilers.
type ServiceTestcase struct {
	New   func(context.Context, testing.TB, client.Client) client.Object
	Check CheckFunc
	Name  string
}

// CheckFunc is a function called to verify the expected state of the (fake)
// cluster.
//
// The passed-in name is the name of the created resouce, and the the passed-in
// condition is the `Available` condition.
type CheckFunc func(ctx context.Context, t testing.TB, c client.Client, name types.NamespacedName, cnd *metav1.Condition) (ok bool)

// Run does what it says on the tin.
//
// Intended to be used to drive subtests.
func (tc ServiceTestcase) Run(ctx context.Context, c client.Client) func(*testing.T) {
	return func(t *testing.T) {
		o := tc.New(ctx, t, c)
		if err := c.Create(ctx, o); err != nil {
			t.Error(err)
		}
		t.Logf("created: %q", o.GetName())
		// If this can't succeed, this code deserves to panic.
		typ := reflect.TypeOf(o).Elem()

		lookup := types.NamespacedName{
			Name:      o.GetName(),
			Namespace: o.GetNamespace(),
		}
		retryCheck(ctx, t, c, lookup, typ, tc.Check)
	}
}

func configSetup(ctx context.Context, t testing.TB, c client.Client) *clairv1alpha1.ConfigReference {
	cfg := corev1.ConfigMap{}
	cfg.GenerateName = "test-config-"
	cfg.Namespace = "default"
	// Don't need extra annotations, because we're dodging the webhooks.
	if err := c.Create(ctx, &cfg); err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() {
		if err := c.Delete(ctx, &cfg); err != nil {
			t.Log(err)
		}
	})
	t.Logf("created ConfigMap: %s", cfg.Name)

	return &clairv1alpha1.ConfigReference{
		Name:     cfg.GetName(),
		APIGroup: new(string), // APIGroup is "core", i.e. empty
		Kind:     "ConfigMap",
	}
}

func markDeploymentAvailable(ctx context.Context, t testing.TB, c client.Client, cur client.Object, refs []corev1.TypedLocalObjectReference) {
	n := types.NamespacedName{
		Namespace: cur.GetNamespace(),
	}
	for _, ref := range refs {
		if ref.Kind != "Deployment" {
			continue
		}
		n.Name = ref.Name
		var d appsv1.Deployment
		if err := c.Get(ctx, n, &d); err != nil {
			t.Error(err)
			return
		}
		upd := d.DeepCopy()
		upd.Status.Conditions = append(upd.Status.Conditions, appsv1.DeploymentCondition{
			Type:   appsv1.DeploymentAvailable,
			Status: corev1.ConditionTrue,
			Reason: "TestTransition",
		})
		if err := c.Status().Update(ctx, upd); err != nil {
			t.Error(err)
			return
		}
		break
	}
	if n.Name == "" {
		t.Errorf("unable to find Deployment ref on %q", cur.GetName())
	}
}

func retryCheck(ctx context.Context, t testing.TB, c client.Client, name types.NamespacedName, typ reflect.Type, check CheckFunc) {
	t.Helper()

	timeout := time.After(time.Minute)
	interval := time.NewTicker(time.Second)
	defer interval.Stop()
	for ct := 0; ; ct++ {
		o := reflect.New(typ).Interface().(client.Object)
		var err error
		select {
		case <-timeout:
			t.Error("timeout")
			return
		case <-interval.C:
			err = c.Get(ctx, name, o)
		}
		if err != nil {
			t.Log(err)
			continue
		}
		s := getStatus(o)
		if len(s.Refs) == 0 {
			t.Log("no refs on object")
		}
		t.Logf("status: %+v", s)
		var status *metav1.Condition
		for _, s := range s.Conditions {
			if s.Type == `Available` {
				status = &s
				break
			}
		}
		if status == nil {
			t.Log("no status")
			continue
		}
		if check(ctx, t, c, name, status) {
			return
		}
		if ct > 10 {
			t.Fatal("more than 10 loops, something's up")
		}
	}
}
