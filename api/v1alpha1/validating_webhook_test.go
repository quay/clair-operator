package v1alpha1

import (
	"context"
	"testing"

	"sigs.k8s.io/controller-runtime/pkg/client"
)

func testValidating(ctx context.Context, c client.Client) func(*testing.T) {
	const (
		key = `config.yaml`
	)

	tt := []webhookTestcase{
		{
			Name: "InvalidConfigWithoutMetadata",
			Setup: func(_ testing.TB, o ConfigObject) {
				o.SetItem(key, invalidConfig)
			},
		},
		{
			Name: "ValidConfigWithMetadata",
			Setup: func(_ testing.TB, o ConfigObject) {
				o.SetItem(key, simpleConfig)
				o.SetLabels(map[string]string{ConfigLabel: ConfigLabelV1})
				o.SetAnnotations(map[string]string{ConfigKey: key})
			},
		},
		{
			Name: "InvalidYAMLWithMetadata",
			Setup: func(_ testing.TB, o ConfigObject) {
				o.SetItem(key, invalidYAML)
				o.SetLabels(map[string]string{ConfigLabel: ConfigLabelV1})
				o.SetAnnotations(map[string]string{ConfigKey: key})
			},
			Check: CheckErr,
		},
		{
			Name: "InvalidConfigWithMetadata",
			Setup: func(_ testing.TB, o ConfigObject) {
				o.SetItem(key, invalidConfig)
				o.SetLabels(map[string]string{ConfigLabel: ConfigLabelV1})
				o.SetAnnotations(map[string]string{ConfigKey: key})
			},
			Check: CheckErr,
		},
		{
			Name: "ValidConfigWithoutMetadata",
			Setup: func(_ testing.TB, o ConfigObject) {
				o.SetItem(key, simpleConfig)
				o.SetLabels(map[string]string{ConfigLabel: ConfigLabelV1})
			},
			Check: CheckErr,
		},
		{
			Name: "MissingConfigWithMetadata",
			Setup: func(_ testing.TB, o ConfigObject) {
				o.SetLabels(map[string]string{ConfigLabel: ConfigLabelV1})
				o.SetAnnotations(map[string]string{ConfigKey: key})
			},
			Check: CheckErr,
		},
		{
			Name: "ValidConfigBadVersion",
			Setup: func(_ testing.TB, o ConfigObject) {
				o.SetItem(key, simpleConfig)
				o.SetLabels(map[string]string{ConfigLabel: `v666`})
				o.SetAnnotations(map[string]string{ConfigKey: key})
			},
			Check: CheckErr,
		},
	}

	return func(t *testing.T) {
		t.Run("ConfigMap", func(t *testing.T) {
			for _, tc := range tt {
				t.Run(tc.Name, tc.Run(ctx, c, mkConfigMap(t)))
			}
		})
		t.Run("Secret", func(t *testing.T) {
			for _, tc := range tt {
				t.Run(tc.Name, tc.Run(ctx, c, mkSecret(t)))
			}
		})
	}
}
