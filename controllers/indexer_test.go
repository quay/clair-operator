package controllers

import (
	"context"
	"testing"

	metav1 "k8s.io/apimachinery/pkg/apis/meta/v1"
	"k8s.io/apimachinery/pkg/types"
	"sigs.k8s.io/controller-runtime/pkg/client"

	clairv1alpha1 "github.com/quay/clair-operator/api/v1alpha1"
)

func mkIndexer(ctx context.Context, t testing.TB, c client.Client) client.Object {
	ref := configSetup(ctx, t, c)
	i := clairv1alpha1.Indexer{
		TypeMeta: metav1.TypeMeta{
			APIVersion: clairv1alpha1.GroupVersion.String(),
			Kind:       "Indexer",
		},
		ObjectMeta: metav1.ObjectMeta{
			GenerateName: "test-indexer-",
			Namespace:    "default",
		},
	}
	i.Spec.Config = ref
	return &i
}

func TestIndexer(t *testing.T) {
	ctx, c := EnvSetup(context.Background(), t)
	tt := []ServiceTestcase{
		{
			Name:  "Happy",
			New:   mkIndexer,
			Check: checkIndexerAvailable,
		},
		{
			Name: "Incomplete",
			New: func(_ context.Context, _ testing.TB, _ client.Client) client.Object {
				i := clairv1alpha1.Indexer{
					TypeMeta: metav1.TypeMeta{
						APIVersion: clairv1alpha1.GroupVersion.String(),
						Kind:       "Indexer",
					},
					ObjectMeta: metav1.ObjectMeta{
						GenerateName: "test-indexer-",
						Namespace:    "default",
					},
				}
				return &i
			},
			Check: checkIndexerIncomplete,
		},
	}

	for _, tc := range tt {
		t.Run(tc.Name, tc.Run(ctx, c))
	}
}

func checkIndexerAvailable(ctx context.Context, t testing.TB, c client.Client, name types.NamespacedName, cnd *metav1.Condition) (ok bool) {
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

func checkIndexerIncomplete(ctx context.Context, t testing.TB, c client.Client, name types.NamespacedName, cnd *metav1.Condition) (ok bool) {
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
