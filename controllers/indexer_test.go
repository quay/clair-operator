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
	var err error
Retry:
	for {
		select {
		case <-timeout:
			t.Error("timeout")
			break Retry
		case <-interval.C:
			err = c.Get(ctx, lookup, &created)
		}
		if err != nil {
			t.Log(err)
		} else {
			break Retry
		}
	}
	t.Logf("looked up: %q", created.GetName())
}
