package v1alpha1

import (
	. "github.com/onsi/ginkgo"
	. "github.com/onsi/gomega"
	corev1 "k8s.io/api/core/v1"
)

var _ = Describe("config mutation webhook", func() {
	const (
		inKey  = `config.yaml.in`
		outKey = `config.yaml`
	)
	Context("should not reject", func() {
		const validConfig = `---
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
		Context("a noop", func() {
			Specify("ConfigMap", func() {
				cm := &corev1.ConfigMap{}
				cm.GenerateName = "mut-test"
				cm.Namespace = "default"
				cm.Data = map[string]string{inKey: validConfig}
				cm.Labels = map[string]string{ConfigLabel: ConfigLabelV1}
				cm.Annotations = map[string]string{
					TemplateKey: inKey,
					ConfigKey:   outKey,
				}

				err := k8sClient.Create(ctx, cm)
				Expect(err).ShouldNot(HaveOccurred())
			})
			Specify("Secret", func() {
				s := &corev1.Secret{}
				s.GenerateName = "mut-test"
				s.Namespace = "default"
				s.StringData = map[string]string{inKey: validConfig}
				s.Labels = map[string]string{ConfigLabel: ConfigLabelV1}
				s.Annotations = map[string]string{
					TemplateKey: inKey,
					ConfigKey:   outKey,
				}

				err := k8sClient.Create(ctx, s)
				Expect(err).ShouldNot(HaveOccurred())
			})
		})

		Context("guessed template outputs", func() {
			Specify(inKey, func() {
				cm := &corev1.ConfigMap{}
				cm.GenerateName = "mut-test"
				cm.Namespace = "default"
				cm.Data = map[string]string{inKey: validConfig}
				cm.Labels = map[string]string{ConfigLabel: ConfigLabelV1}
				cm.Annotations = map[string]string{
					TemplateKey: inKey,
				}

				err := k8sClient.Create(ctx, cm)
				Expect(err).ShouldNot(HaveOccurred())
			})
			const noext = `config`
			Specify(noext, func() {
				cm := &corev1.ConfigMap{}
				cm.GenerateName = "mut-test"
				cm.Namespace = "default"
				cm.Data = map[string]string{noext: validConfig}
				cm.Labels = map[string]string{ConfigLabel: ConfigLabelV1}
				cm.Annotations = map[string]string{
					TemplateKey: noext,
				}

				err := k8sClient.Create(ctx, cm)
				Expect(err).ShouldNot(HaveOccurred())
			})
		})
	})

	Context("should reject", func() {
		Specify("missing input key", func() {
			cm := &corev1.ConfigMap{}
			cm.GenerateName = "mut-test"
			cm.Namespace = "default"
			//cm.Data = map[string]string{inKey: validConfig}
			cm.Labels = map[string]string{ConfigLabel: ConfigLabelV1}
			cm.Annotations = map[string]string{
				TemplateKey: inKey,
				ConfigKey:   outKey,
			}

			err := k8sClient.Create(ctx, cm)
			Expect(err).Should(HaveOccurred())
		})
		Specify("invalid YAML", func() {
			cm := &corev1.ConfigMap{}
			cm.GenerateName = "mut-test"
			cm.Namespace = "default"
			cm.Data = map[string]string{inKey: "}\n\t\tbad:yaml\n"}
			cm.Labels = map[string]string{ConfigLabel: ConfigLabelV1}
			cm.Annotations = map[string]string{
				TemplateKey: inKey,
				ConfigKey:   outKey,
			}

			err := k8sClient.Create(ctx, cm)
			Expect(err).Should(HaveOccurred())
		})
	})

	Context("should render", func() {
		It("needs to construct the database", func() {
			db := &corev1.Secret{}
			db.Name = "mutation-database-creds"
			db.Namespace = "default"
			db.StringData = map[string]string{
				`PGHOST`:     `localhost`,
				`PGDATABASE`: `clair`,
				`PGUSER`:     `clair`,
				`PGPASSWORD`: `verysecret`,
			}

			err := k8sClient.Create(ctx, db)
			Expect(err).ShouldNot(HaveOccurred())
		})

		Specify("indexer secret", func() {
			const config = `---
indexer:
  connstring: secret:default/mutation-database-creds
matcher:
  connstring: veryrealdatabase
  indexer_addr: "http://clair"
notifier:
  connstring: veryrealdatabase
  indexer_addr: "http://clair"
  matcher_addr: "http://clair"
`

			s := &corev1.Secret{}
			s.GenerateName = "mut-test"
			s.Namespace = "default"
			s.StringData = map[string]string{inKey: config}
			s.Labels = map[string]string{ConfigLabel: ConfigLabelV1}
			s.Annotations = map[string]string{
				TemplateKey: inKey,
				ConfigKey:   outKey,
			}

			err := k8sClient.Create(ctx, s)
			Expect(err).ShouldNot(HaveOccurred())
		})
	})
})
