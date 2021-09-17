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

	t.Run("ConfigMap", func(t *testing.T) {
		var cfg unstructured.Unstructured
		cfg.SetGroupVersionKind(schema.GroupVersionKind{
			Version: "v1",
			Kind:    "ConfigMap",
		})
		cfg.SetName("injected-cfg")
		for _, tc := range []templateTestcase{
			{
				Name: "Indexer",
				Mk:   k.Indexer,
				Img:  "test/image:tag",
				Want: "testdata/want.config.indexer.yaml",
			},
			{
				Name: "Matcher",
				Mk:   k.Matcher,
				Img:  "test/image:tag",
				Want: "testdata/want.config.matcher.yaml",
			},
			{
				Name: "Notifier",
				Mk:   k.Notifier,
				Img:  "test/image:tag",
				Want: "testdata/want.config.notifier.yaml",
			},
		} {
			t.Run(tc.Name, tc.Run(&cfg))
		}
	})

	t.Run("Secret", func(t *testing.T) {
		var cfg unstructured.Unstructured
		cfg.SetGroupVersionKind(schema.GroupVersionKind{
			Version: "v1",
			Kind:    "Secret",
		})
		cfg.SetName("injected-secret")
		for _, tc := range []templateTestcase{
			{
				Name: "Indexer",
				Mk:   k.Indexer,
				Img:  "test/image:tag",
				Want: "testdata/want.secret.indexer.yaml",
			},
			{
				Name: "Matcher",
				Mk:   k.Matcher,
				Img:  "test/image:tag",
				Want: "testdata/want.secret.matcher.yaml",
			},
			{
				Name: "Notifier",
				Mk:   k.Notifier,
				Img:  "test/image:tag",
				Want: "testdata/want.secret.notifier.yaml",
			},
		} {
			t.Run(tc.Name, tc.Run(&cfg))
		}
	})
}

type templateTestcase struct {
	Name string
	Mk   func(cfg configObject, img string) (resmap.ResMap, error)
	Img  string
	Want string
}

func (tc templateTestcase) Run(cfg *unstructured.Unstructured) func(*testing.T) {
	return func(t *testing.T) {
		f, err := os.Open(tc.Want)
		if err != nil {
			t.Fatal(err)
		}
		defer f.Close()
		wantb, err := io.ReadAll(f)
		if err != nil {
			t.Fatal(err)
		}

		res, err := tc.Mk(cfg, tc.Img)
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
	}
}

func (k *kustomize) Indexer(cfg configObject, image string) (resmap.ResMap, error) {
	return k.Run(cfg, "indexer", image)
}

func (k *kustomize) Matcher(cfg configObject, image string) (resmap.ResMap, error) {
	return k.Run(cfg, "matcher", image)
}

func (k *kustomize) Notifier(cfg configObject, image string) (resmap.ResMap, error) {
	return k.Run(cfg, "notifier", image)
}
