---
id: C-656
title: "SSO member login to a hosted Exchange"
pillar: "Core"
status: backlog
epic: hosted-single-org
areas: [flux-cli, flux-credentials]
note: "Decision 0019 rule 5: browser OIDC sign-in, member token mint, token stored under the named binding credential reference; replaces the transitional env pair"
---

# SSO member login to a hosted Exchange

## Goal

One login journey binds a laptop Flux to a hosted single-org Exchange (Decision 0019 rule 5). The
user signs in through the deployment's OIDC provider in the browser, the Exchange mints a member
Service Account token under the Decision 0019 rule 2 policy, and the token lands in the Flux
credential store under the named exchange binding's credential reference (C-650). The transitional
`FLUX_EXCHANGE_URL`/`FLUX_EXCHANGE_SERVICE_ACCOUNT_TOKEN` pair retires once this journey ships.

## Acceptance

- [ ] Login against a named exchange binding completes browser OIDC sign-in plus member mint and
      stores the token in the credential store; the token never appears in argv, configuration,
      environment output or a JSON face.
- [ ] The embedded Exchange client resolves origin and token from the named binding; the env pair
      keeps working with a deprecation warning, and a conflict prefers the explicit binding.
- [ ] The token value is absent from logs, events, session state and error faces; the redactor is
      seeded from the credential-store read; a leak test proves absence.
- [ ] Depends on the Decision 0019 rule 2 Exchange member-mint story and on C-650; not
      dispatchable before them.
