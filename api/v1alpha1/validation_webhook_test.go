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

var _ = Describe("config validation webhook", func() {
	const key = `config.yaml`
	Context("should not reject", func() {
		Specify("an invalid ConfigMap without the label", func() {
			cm := &corev1.ConfigMap{}
			cm.Name = "invalid-unlabeled"
			cm.Namespace = "default"
			cm.Data = map[string]string{key: invalidConfig}

			err := k8sClient.Create(ctx, cm)
			Expect(err).ShouldNot(HaveOccurred())
		})
		Specify("an invalid Secret without the label", func() {
			s := &corev1.Secret{}
			s.Name = "invalid-unlabeled"
			s.Namespace = "default"
			s.StringData = map[string]string{key: invalidConfig}

			err := k8sClient.Create(ctx, s)
			Expect(err).ShouldNot(HaveOccurred())
		})

		Specify("a valid ConfigMap with valid metadata", func() {
			cm := &corev1.ConfigMap{}
			cm.Name = "valid-valid"
			cm.Namespace = "default"
			cm.Data = map[string]string{key: validConfig}
			cm.Labels = map[string]string{ConfigLabel: ConfigLabelV1}
			cm.Annotations = map[string]string{ConfigAnnotation: key}

			err := k8sClient.Create(ctx, cm)
			Expect(err).ShouldNot(HaveOccurred())
		})
		Specify("a valid Secret with valid metadata", func() {
			s := &corev1.Secret{}
			s.Name = "valid-valid"
			s.Namespace = "default"
			s.StringData = map[string]string{key: validConfig}
			s.Labels = map[string]string{ConfigLabel: ConfigLabelV1}
			s.Annotations = map[string]string{ConfigAnnotation: key}

			err := k8sClient.Create(ctx, s)
			Expect(err).ShouldNot(HaveOccurred())
		})
	})

	Context("should reject", func() {
		Specify("an invalid ConfigMap with valid metadata", func() {
			cm := &corev1.ConfigMap{}
			cm.Name = "invalid-valid"
			cm.Namespace = "default"
			cm.Data = map[string]string{key: invalidConfig}
			cm.Labels = map[string]string{ConfigLabel: ConfigLabelV1}
			cm.Annotations = map[string]string{ConfigAnnotation: key}

			err := k8sClient.Create(ctx, cm)
			Expect(err).Should(HaveOccurred())
		})
		Specify("an invalid Secret with valid metadata", func() {
			s := &corev1.Secret{}
			s.Name = "invalid-valid"
			s.Namespace = "default"
			s.StringData = map[string]string{key: invalidConfig}
			s.Labels = map[string]string{ConfigLabel: ConfigLabelV1}
			s.Annotations = map[string]string{ConfigAnnotation: key}

			err := k8sClient.Create(ctx, s)
			Expect(err).Should(HaveOccurred())
		})

		Specify("a valid ConfigMap with the label, but no annotation", func() {
			cm := &corev1.ConfigMap{}
			cm.Name = "valid-labeled-unannotated"
			cm.Namespace = "default"
			cm.Data = map[string]string{key: validConfig}
			cm.Labels = map[string]string{ConfigLabel: ConfigLabelV1}

			err := k8sClient.Create(ctx, cm)
			Expect(err).Should(HaveOccurred())
		})
	})
})
