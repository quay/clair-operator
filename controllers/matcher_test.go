package controllers

import (
	"context"
	"testing"

	clairv1alpha1 "github.com/quay/clair-operator/api/v1alpha1"
	metav1 "k8s.io/apimachinery/pkg/apis/meta/v1"
	"k8s.io/apimachinery/pkg/types"
	"sigs.k8s.io/controller-runtime/pkg/client"
)

func mkMatcher(ctx context.Context, t testing.TB, c client.Client) client.Object {
	ref := configSetup(ctx, t, c)
	m := clairv1alpha1.Matcher{
		TypeMeta: metav1.TypeMeta{
			APIVersion: clairv1alpha1.GroupVersion.String(),
			Kind:       "Matcher",
		},
		ObjectMeta: metav1.ObjectMeta{
			GenerateName: "test-matcher-",
			Namespace:    "default",
		},
	}
	m.Spec.Config = ref
	return &m
}

func TestMatcher(t *testing.T) {
	ctx, c := EnvSetup(context.Background(), t)

	tt := []ServiceTestcase{
		{
			Name: "Simple",
			New:  mkMatcher,
			Check: func(ctx context.Context, t testing.TB, c client.Client, name types.NamespacedName, cnd *metav1.Condition) (ok bool) {
				m := clairv1alpha1.Matcher{}
				if cnd.Status == metav1.ConditionTrue {
					t.Log("indexer marked available")
					if err := c.Get(ctx, name, &m); err != nil {
						t.Log(err)
						return false
					}
					for _, ref := range m.Status.Refs {
						t.Logf("found: %v", ref)
					}
					return true
				}
				switch cnd.Reason {
				case `DeploymentUnavailable`:
					t.Log("marking Deployment available")
					if err := c.Get(ctx, name, &m); err != nil {
						t.Log(err)
						return false
					}
					markDeploymentAvailable(ctx, t, c, &m, m.Status.Refs)
				default:
					t.Errorf("unknown reason: %q", cnd.Reason)
				}
				return false
			},
		},
	}
	for _, tc := range tt {
		t.Run(tc.Name, tc.Run(ctx, c))
	}
}
