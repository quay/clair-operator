package controllers

import (
	"io"
	"os"
	"testing"

	"github.com/google/go-cmp/cmp"
	"k8s.io/apimachinery/pkg/apis/meta/v1/unstructured"
	"k8s.io/apimachinery/pkg/runtime/schema"
	"sigs.k8s.io/kustomize/api/resmap"
)

func TestTemplate(t *testing.T) {
	t.Parallel()
	k, err := newKustomize()
	if err != nil {
		t.Fatal(err)
	}
	cfg := &unstructured.Unstructured{}
	cfg.SetGroupVersionKind(schema.GroupVersionKind{
		Version: "v1",
		Kind:    "ConfigMap",
	})
	cfg.SetName("injected-cfg")

	tt := []struct {
		name string
		mk   func(cfg configObject) (resmap.ResMap, error)
		gold string
	}{
		{
			name: "Indexer",
			mk:   k.Indexer,
			gold: "testdata/want.indexer.yaml",
		},
	}

	for i := range tt {
		tc := &tt[i]
		t.Run(tc.name, func(t *testing.T) {
			f, err := os.Open(tc.gold)
			if err != nil {
				t.Fatal(err)
			}
			defer f.Close()
			wantb, err := io.ReadAll(f)
			if err != nil {
				t.Fatal(err)
			}

			res, err := tc.mk(cfg)
			if err != nil {
				t.Error(err)
			}
			if res == nil {
				return
			}
			gotb, err := res.AsYaml()
			if err != nil {
				t.Error(err)
			}
			if got, want := string(gotb), string(wantb); !cmp.Equal(want, got) {
				t.Error(cmp.Diff(want, got))
			}
		})
	}
}
