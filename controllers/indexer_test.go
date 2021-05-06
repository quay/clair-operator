package controllers

import (
	"context"
	"testing"
	"time"

	metav1 "k8s.io/apimachinery/pkg/apis/meta/v1"
	"k8s.io/apimachinery/pkg/types"
	"sigs.k8s.io/controller-runtime/pkg/client"

	clairv1alpha1 "github.com/quay/clair-operator/api/v1alpha1"
)

func TestIndexer(t *testing.T) {
	ctx, c := envSetup(context.Background(), t)
	t.Run("Happy", func(t *testing.T) {
		ref := configSetup(ctx, t, c)
		obj := clairv1alpha1.Indexer{
			TypeMeta: metav1.TypeMeta{
				APIVersion: clairv1alpha1.GroupVersion.String(),
				Kind:       "Indexer",
			},
			ObjectMeta: metav1.ObjectMeta{
				GenerateName: "test-indexer-",
				Namespace:    "default",
			},
			Spec: clairv1alpha1.IndexerSpec{
				Config: ref,
			},
		}
		if err := c.Create(ctx, &obj); err != nil {
			t.Error(err)
		}
		t.Logf("created: %q", obj.GetName())

		lookup := types.NamespacedName{Name: obj.Name, Namespace: obj.Namespace}
		retryCheck(ctx, t, c, lookup, checkAvailable)

	})

	t.Run("Incomplete", func(t *testing.T) {
		obj := clairv1alpha1.Indexer{
			TypeMeta: metav1.TypeMeta{
				APIVersion: clairv1alpha1.GroupVersion.String(),
				Kind:       "Indexer",
			},
			ObjectMeta: metav1.ObjectMeta{
				GenerateName: "test-indexer-",
				Namespace:    "default",
			},
			Spec: clairv1alpha1.IndexerSpec{
				Config: nil,
			},
		}
		if err := c.Create(ctx, &obj); err != nil {
			t.Error(err)
		}
		t.Logf("created: %q", obj.GetName())

		lookup := types.NamespacedName{Name: obj.Name, Namespace: obj.Namespace}
		retryCheck(ctx, t, c, lookup, checkIncomplete)
	})
}

func retryCheck(ctx context.Context, t testing.TB, c client.Client, name types.NamespacedName, check checkFunc) {
	t.Helper()
	o := clairv1alpha1.Indexer{}

	timeout := time.After(time.Minute)
	interval := time.NewTicker(time.Second)
	defer interval.Stop()
	for ct := 0; ; ct++ {
		var err error
		select {
		case <-timeout:
			t.Error("timeout")
			return
		case <-interval.C:
			err = c.Get(ctx, name, &o)
		}
		if err != nil {
			t.Log(err)
			continue
		}
		if len(o.Status.Refs) == 0 {
			t.Log("no refs on object")
		}
		t.Logf("status: %+v", o.Status)
		var status *metav1.Condition
		for _, s := range o.Status.Conditions {
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

type checkFunc func(ctx context.Context, t testing.TB, c client.Client, name types.NamespacedName, cnd *metav1.Condition) (ok bool)

func checkAvailable(ctx context.Context, t testing.TB, c client.Client, name types.NamespacedName, cnd *metav1.Condition) (ok bool) {
	i := clairv1alpha1.Indexer{}
	if cnd.Status == metav1.ConditionTrue {
		t.Log("indexer marked available")
		if err := c.Get(ctx, name, &i); err != nil {
			t.Log(err)
			return false
		}
		for _, ref := range i.Status.Refs {
			t.Logf("found: %v", ref)
		}
		return true
	}
	switch cnd.Reason {
	case `DeploymentUnavailable`:
		t.Log("marking Deployment available")
		if err := c.Get(ctx, name, &i); err != nil {
			t.Log(err)
			return false
		}
		markDeploymentAvailable(ctx, t, c, &i, i.Status.Refs)
	default:
		t.Errorf("unknown reason: %q", cnd.Reason)
	}
	return false
}

func checkIncomplete(ctx context.Context, t testing.TB, c client.Client, name types.NamespacedName, cnd *metav1.Condition) (ok bool) {
	switch cnd.Status {
	case metav1.ConditionFalse:
		if cnd.Reason == "InvalidSpec" {
			return true
		}
	case metav1.ConditionTrue:
	case metav1.ConditionUnknown:
	}
	return false
}
