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
	"testing"

	"github.com/go-logr/zapr"
	"go.uber.org/zap/zapcore"
	"go.uber.org/zap/zaptest"
	appsv1 "k8s.io/api/apps/v1"
	scalev2 "k8s.io/api/autoscaling/v2beta2"
	corev1 "k8s.io/api/core/v1"
	"k8s.io/apimachinery/pkg/runtime"
	"k8s.io/apimachinery/pkg/types"
	ctrl "sigs.k8s.io/controller-runtime"
	"sigs.k8s.io/controller-runtime/pkg/client"
	"sigs.k8s.io/controller-runtime/pkg/envtest"
	"sigs.k8s.io/controller-runtime/pkg/log"

	clairv1alpha1 "github.com/quay/clair-operator/api/v1alpha1"
	// +kubebuilder:scaffold:imports
)

func envSetup(ctx context.Context, t testing.TB) (context.Context, client.Client) {
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
	mgrctx, mgrdone := context.WithCancel(ctx)
	go func() {
		if err := mgr.Start(mgrctx); err != nil {
			t.Errorf("error from Manager.Start: %v", err)
		}
	}()
	t.Cleanup(mgrdone)

	return ctx, mgr.GetClient()
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
		APIGroup: new(string), // APIGroup is "core", e.g. empty
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
