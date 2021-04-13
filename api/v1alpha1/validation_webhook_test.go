package v1alpha1

import (
	. "github.com/onsi/ginkgo"
	. "github.com/onsi/gomega"
	corev1 "k8s.io/api/core/v1"
)

const (
	invalidConfig = `{
not legal
	yaml
at: all
`
	validConfig = `---
matcher:
  conn_string: veryrealdatabase
`
)

var _ = Describe("config validation webhook", func() {
	Context("should not reject", func() {
		Specify("an invalid ConfigMap without the label", func() {
			cm := &corev1.ConfigMap{}
			cm.Name = "invalid-unlabeled"
			cm.Namespace = "default"
			cm.Data = map[string]string{
				"config.yaml": invalidConfig,
			}

			err := k8sClient.Create(ctx, cm)
			Expect(err).ShouldNot(HaveOccurred())
		})
		Specify("an invalid Secret without the label", func() {
			s := &corev1.Secret{}
			s.Name = "invalid-unlabeled"
			s.Namespace = "default"
			s.StringData = map[string]string{
				"config.yaml": invalidConfig,
			}

			err := k8sClient.Create(ctx, s)
			Expect(err).ShouldNot(HaveOccurred())
		})
		Specify("a valid ConfigMap with the label", func() {
			cm := &corev1.ConfigMap{}
			cm.Name = "valid-labeled"
			cm.Namespace = "default"
			cm.Data = map[string]string{
				"config.yaml": validConfig,
			}
			cm.Labels = map[string]string{
				ConfigLabel: ConfigLabelV1,
			}

			err := k8sClient.Create(ctx, cm)
			Expect(err).ShouldNot(HaveOccurred())
		})
		Specify("a valid Secret with the label", func() {
			s := &corev1.Secret{}
			s.Name = "valid-labeled"
			s.Namespace = "default"
			s.StringData = map[string]string{
				"config.yaml": validConfig,
			}
			s.Labels = map[string]string{
				ConfigLabel: ConfigLabelV1,
			}

			err := k8sClient.Create(ctx, s)
			Expect(err).ShouldNot(HaveOccurred())
		})
	})

	Context("should reject", func() {
		Specify("an invalid ConfigMap with the label", func() {
			cm := &corev1.ConfigMap{}
			cm.Name = "invalid-unlabeled"
			cm.Namespace = "default"
			cm.Data = map[string]string{
				"config.yaml": invalidConfig,
			}
			cm.Labels = map[string]string{
				ConfigLabel: ConfigLabelV1,
			}

			err := k8sClient.Create(ctx, cm)
			Expect(err).Should(HaveOccurred())
		})
		Specify("an invalid Secret with the label", func() {
			s := &corev1.Secret{}
			s.Name = "invalid-unlabeled"
			s.Namespace = "default"
			s.StringData = map[string]string{
				"config.yaml": invalidConfig,
			}
			s.Labels = map[string]string{
				ConfigLabel: ConfigLabelV1,
			}

			err := k8sClient.Create(ctx, s)
			Expect(err).Should(HaveOccurred())
		})
	})
})
