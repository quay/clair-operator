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
		inKey  = `config.yaml.in`
		outKey = `config.yaml`
		noext  = `config`
	)
	noopConfig := simpleConfig

	dbData := map[string]string{
		`PGHOST`:     `localhost`,
		`PGDATABASE`: `clair`,
		`PGUSER`:     `clair`,
		`PGPASSWORD`: `verysecret`,
		`PGSSLMODE`:  `disable`,
	}
	deps := []client.Object{
		func() client.Object {
			o := &corev1.Secret{}
			o.Name = "mutation-database-creds"
			o.Namespace = "default"
			o.StringData = dbData
			return o
		}(),
		func() client.Object {
			o := &corev1.ConfigMap{}
			o.Name = "mutation-database-creds"
			o.Namespace = "default"
			o.Data = dbData
			return o
		}(),
		func() client.Object {
			o := &corev1.Service{}
			o.Name = "other-clair"
			o.Namespace = "default"
			o.Spec.Ports = []corev1.ServicePort{
				{
					Name: PortAPI,
					Port: 6060,
				},
				{
					Name: PortIntrospection,
					Port: 8080,
				},
			}
			return o
		}(),
	}

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
		{
			Name: "ValidConfigBadVersion",
			Setup: func(_ testing.TB, o ConfigObject) {
				o.SetItem(inKey, noopConfig)
				o.SetLabels(map[string]string{ConfigLabel: `v666`})
				o.SetAnnotations(map[string]string{
					TemplateKey: inKey,
					ConfigKey:   outKey,
				})
			},
			Check: CheckErr,
		},
		{
			Name: "Rendering",
			Setup: func(_ testing.TB, o ConfigObject) {
				o.SetItem(inKey, allRefConfig)
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
				got, want := o.GetItem(outKey), allRefConfigRendered
				if !cmp.Equal(got, want) {
					t.Error(cmp.Diff(got, want))
				}
			},
		},
		{
			Name: "RenderingBadRefs",
			Setup: func(_ testing.TB, o ConfigObject) {
				o.SetItem(inKey, allRefIncorrect)
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
				got, want := o.GetItem(outKey), allRefIncorrect
				if !cmp.Equal(got, want) {
					t.Error(cmp.Diff(got, want))
				}
			},
		},
	}

	return func(t *testing.T) {
		// Do some common setup
		for i := range deps {
			if err := c.Create(ctx, deps[i]); err != nil {
				t.Fatal(err)
			}
		}
		t.Cleanup(func() {
			for i := range deps {
				if err := c.Delete(ctx, deps[i]); err != nil {
					t.Error(err)
				}
			}
		})

		t.Run("ConfigMap", func(t *testing.T) {
			var lt = []webhookTestcase{
				{
					Name: "ConfigWithSecret",
					Setup: func(_ testing.TB, o ConfigObject) {
						o.SetItem(inKey, secretRefConfig)
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
						o.SetItem(inKey, secretRefConfig)
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
						got, want := o.GetItem(outKey), secretRefConfigRendered
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
