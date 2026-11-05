## Summary
The meeting focused on finalizing the release schedule for the "Two Point Four" version, resolving technical debt regarding Salesforce integration and billing database architecture, and securing a budget for a new designer. Key decisions include pushing the release to November 14th, deferring Salesforce integration to January, and moving the billing database to PostgreSQL. A freelance designer will be hired for three months to address the lack of a dedicated designer role.

## Decisions
- **Release Date:** The "Two Point Four" version will launch on Thursday, November 14th.
- **Salesforce Integration:** The Salesforce integration is postponed to January to allow for dedicated resources and to address the client's urgent needs.
- **Billing Database:** The billing service will use PostgreSQL instead of MongoDB, utilizing JSONB columns to handle country-specific data.
- **Designer Role:** A freelance designer will be hired for three months at a rate of $3,000 per month.
- **Team Pricing:** The decision to use a per-user pricing model for the Team plan will be finalized at the next meeting on November 20th.

## Action items
- **Lucía:** Prepare the "Two Point Four" release notes by the end of the current week (before Wednesday) and mark the new report panel as a beta feature.
- **Martín:** Resolve the duplicate payment webhook ticket (Ticket #412) before the release date; if issues persist on Wednesday, the ticket will be moved to the next meeting.
- **Lucía:** Draft a technical summary for the upcoming call with Acme regarding the Salesforce integration delay.
- **Sofía:** Create a new job posting for a remote designer in Spanish, highlighting the budget and availability, and submit it to the two job boards.
- **Me:** Schedule a meeting with the team on November 20th to review the current team pricing data and finalize the Team plan pricing strategy.

## Open questions
- **Martín:** Will the duplicate payment ticket (Ticket #412) be fully resolved by the end of the current week, or will it be moved to the next meeting?
- **Sofía:** What is the specific timeline for the new designer's onboarding and the completion of the new report panel?
- **Me:** What is the final agreed-upon per-user pricing model for the Team plan, pending the review of the November 20th data?

## Numbers
- **Release Date:** Thursday, November 14th
- **Salesforce Integration:** Postponed to January
- **Billing Database:** PostgreSQL (instead of MongoDB)
- **Designer Hire:** 3 months
- **Designer Rate:** $3,000 per month
- **Team Plan:** Per-user pricing model
- **Churn Rate:** 3.2% (current month)
- **New Clients:** 41 (current month)