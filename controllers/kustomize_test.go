package controllers

import (
	"io"
	"os"
	"testing"

	"github.com/google/go-cmp/cmp"
	"k8s.io/apimachinery/pkg/apis/meta/v1/unstructured"
	"k8s.io/apimachinery/pkg/runtime/schema"
)

func TestIndexerTemplate(t *testing.T) {
	f, err := os.Open("testdata/want.indexer.yaml")
	if err != nil {
		t.Fatal(err)
	}
	defer f.Close()

	k := newKustomize()
	cfg := &unstructured.Unstructured{}
	cfg.SetGroupVersionKind(schema.GroupVersionKind{
		Version: "v1",
		Kind:    "ConfigMap",
	})
	cfg.SetName("injected-cfg")

	res, err := k.Indexer(cfg)
	if err != nil {
		t.Fatal(err)
	}
	gotb, err := res.AsYaml()
	if err != nil {
		t.Error(err)
	}
	wantb, err := io.ReadAll(f)
	if err != nil {
		t.Error(err)
	}
	if got, want := string(gotb), string(wantb); !cmp.Equal(want, got) {
		t.Error(cmp.Diff(want, got))
	}
}
