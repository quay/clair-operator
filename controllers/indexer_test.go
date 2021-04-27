package controllers

import (
	"context"
	"testing"
	"time"

	metav1 "k8s.io/apimachinery/pkg/apis/meta/v1"
	"k8s.io/apimachinery/pkg/types"

	clairv1alpha1 "github.com/quay/clair-operator/api/v1alpha1"
)

func TestIndexer(t *testing.T) {
	ctx, c := envSetup(context.Background(), t)
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

	created := clairv1alpha1.Indexer{}
	lookup := types.NamespacedName{Name: obj.Name, Namespace: obj.Namespace}

	timeout := time.After(time.Minute)
	interval := time.NewTicker(time.Second)
	defer interval.Stop()
Retry:
	for ct := 0; ; ct++ {
		var err error
		select {
		case <-timeout:
			t.Error("timeout")
			break Retry
		case <-interval.C:
			err = c.Get(ctx, lookup, &created)
		}
		if err != nil {
			t.Log(err)
			continue
		}
		if len(created.Status.Refs) == 0 {
			t.Log("no refs on object")
		}
		t.Logf("status: %+v", created.Status)
		var status *metav1.Condition
		for _, s := range created.Status.Conditions {
			if s.Type == `Available` {
				status = &s
				break
			}
		}
		if status == nil {
			t.Log("no status")
			continue
		}
		if status.Status == metav1.ConditionTrue {
			t.Log("indexer marked available")
			break Retry
		}
		switch status.Reason {
		case `DeploymentUnavailable`:
			t.Log("marking Deployment available")
			markDeploymentAvailable(ctx, t, c, &created, created.Status.Refs)
		default:
			t.Errorf("unknown reason: %q", status.Reason)
		}
		if ct > 10 {
			t.Fatal("more than 10 loops, something's up")
		}
	}
	t.Logf("looked up: %q", created.GetName())

	for _, ref := range created.Status.Refs {
		t.Logf("found: %v", ref)
	}
}
