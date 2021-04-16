package v1alpha1

import (
	"context"
	"testing"

	"github.com/google/go-cmp/cmp"
	corev1 "k8s.io/api/core/v1"
	"sigs.k8s.io/controller-runtime/pkg/client"
)

func testMutating(ctx context.Context, c client.Client) func(*testing.T) {
	const (
		inKey      = `config.yaml.in`
		outKey     = `config.yaml`
		noext      = `config`
		noopConfig = `---
indexer:
  connstring: veryrealdatabase
matcher:
  connstring: veryrealdatabase
  indexer_addr: "http://clair"
notifier:
  connstring: veryrealdatabase
  indexer_addr: "http://clair"
  matcher_addr: "http://clair"
`
	)

	tt := []webhookTestcase{
		{
			Name: "NoopConfigWithMetadata",
			Setup: func(_ testing.TB, o ConfigObject) {
				o.SetItem(inKey, noopConfig)
				o.SetLabels(map[string]string{ConfigLabel: ConfigLabelV1})
				o.SetAnnotations(map[string]string{
					TemplateKey: inKey,
					ConfigKey:   outKey,
				})
			},
		},
		{
			Name: "NoopConfigWithGuessedOutput",
			Setup: func(_ testing.TB, o ConfigObject) {
				o.SetItem(inKey, noopConfig)
				o.SetLabels(map[string]string{ConfigLabel: ConfigLabelV1})
				o.SetAnnotations(map[string]string{
					TemplateKey: inKey,
				})
			},
			Check: func(t testing.TB, o ConfigObject, err error) {
				if err != nil {
					t.Error(err)
				}
				if got := o.GetItem(outKey); got == "" {
					t.Errorf("got: %q, wanted not-empty string", got)
				}
			},
		},
		{
			Name: "NoopConfigWithGuessedOutputOddName",
			// Same as previous, but with a dumb input name
			Setup: func(_ testing.TB, o ConfigObject) {
				o.SetItem(noext, noopConfig)
				o.SetLabels(map[string]string{ConfigLabel: ConfigLabelV1})
				o.SetAnnotations(map[string]string{
					TemplateKey: noext,
				})
			},
			Check: func(t testing.TB, o ConfigObject, err error) {
				if err != nil {
					t.Error(err)
				}
				if got := o.GetItem(outKey); got == "" {
					t.Errorf("got: %q, wanted not-empty string", got)
				}
			},
		},
		{
			Name: "MissingInputKey",
			Setup: func(_ testing.TB, o ConfigObject) {
				o.SetLabels(map[string]string{ConfigLabel: ConfigLabelV1})
				o.SetAnnotations(map[string]string{
					TemplateKey: inKey,
					ConfigKey:   outKey,
				})
			},
			Check: CheckErr,
		},
		{
			Name: "InvalidYAML",
			Setup: func(_ testing.TB, o ConfigObject) {
				o.SetItem(inKey, "}\n\t\tbad:yaml\n")
				o.SetLabels(map[string]string{ConfigLabel: ConfigLabelV1})
				o.SetAnnotations(map[string]string{
					TemplateKey: inKey,
					ConfigKey:   outKey,
				})
			},
			Check: CheckErr,
		},
	}

	return func(t *testing.T) {
		// Do some common setup
		dbsec := &corev1.Secret{}
		dbsec.Name = "mutation-database-creds"
		dbsec.Namespace = "default"
		dbsec.StringData = map[string]string{
			`PGHOST`:     `localhost`,
			`PGDATABASE`: `clair`,
			`PGUSER`:     `clair`,
			`PGPASSWORD`: `verysecret`,
		}
		if err := c.Create(ctx, dbsec); err != nil {
			t.Fatal(err)
		}
		t.Cleanup(func() {
			if err := c.Delete(ctx, dbsec); err != nil {
				t.Error(err)
			}
		})
		const (
			secretConfig = `---
indexer:
  connstring: database+postgres:secret:default/mutation-database-creds
matcher:
  connstring: veryrealdatabase
  indexer_addr: "http://clair"
notifier:
  connstring: veryrealdatabase
  indexer_addr: "http://clair"
  matcher_addr: "http://clair"
`
			renderedSecretConfig = `indexer:
  connstring: postgresql://clair:verysecret@localhost:/clair
matcher:
  connstring: veryrealdatabase
  indexer_addr: "http://clair"
notifier:
  connstring: veryrealdatabase
  indexer_addr: "http://clair"
  matcher_addr: "http://clair"
`
		)

		t.Run("ConfigMap", func(t *testing.T) {
			var lt = []webhookTestcase{
				{
					Name: "ConfigWithSecret",
					Setup: func(_ testing.TB, o ConfigObject) {
						o.SetItem(inKey, secretConfig)
						o.SetLabels(map[string]string{ConfigLabel: ConfigLabelV1})
						o.SetAnnotations(map[string]string{
							TemplateKey: inKey,
							ConfigKey:   outKey,
						})
					},
					Check: CheckErr,
				},
			}
			for _, tc := range append(tt, lt...) {
				t.Run(tc.Name, tc.Run(ctx, c, mkConfigMap(t)))
			}
		})
		t.Run("Secret", func(t *testing.T) {
			var lt = []webhookTestcase{
				{
					Name: "ConfigWithSecret",
					Setup: func(_ testing.TB, o ConfigObject) {
						o.SetItem(inKey, secretConfig)
						o.SetLabels(map[string]string{ConfigLabel: ConfigLabelV1})
						o.SetAnnotations(map[string]string{
							TemplateKey: inKey,
							ConfigKey:   outKey,
						})
					},
					Check: func(t testing.TB, o ConfigObject, err error) {
						if err != nil {
							t.Error(err)
						}
						got, want := o.GetItem(outKey), renderedSecretConfig
						if !cmp.Equal(got, want) {
							t.Error(cmp.Diff(got, want))
						}
					},
				},
			}
			for _, tc := range append(tt, lt...) {
				t.Run(tc.Name, tc.Run(ctx, c, mkSecret(t)))
			}
		})
	}
}
