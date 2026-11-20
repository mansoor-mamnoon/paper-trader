# Follow-up decision audit — 20 November 2026

Evidence: account branch `8c29c36`, the 19 November report, all four holding dossiers,
model transcripts and [daily-run logs](https://github.com/mansoor-mamnoon/paper-trader/actions/runs/44634969870).
The run used main `51a74f1`, including the previous fixes. It queued SELL for AES,
PANW, TJX and TTWO and selected no purchases. No sale had filled when reviewed.
The account still held those four stocks; the note's claim of “100% cash” described
an intended next-open outcome, not the current book.

| Observation | What the evidence establishes | Repair |
|---|---|---|
| AES sale cites an 18-month close timeline. | The report's calendar date is 19 November 2026; the cited headline says closing expected December 2, 2026: 13 days away. The offer premium is not established by the file either. | Explicit calendar date in every stock dossier; factual exit check rejects unsupported timelines and prices. |
| PANW loses its case because no new headline repeats the entry catalyst. | The prior report contains the beat, outlook and acquisition citation, but eight newer stories crowded it out of the dossier. Missing repetition does not establish a broken thesis. | Prioritize material classified news and retain the original entry citation separately, including outside the fresh-buy window. |
| TJX skeptic says the analyst claims 2.8x volume and 5x Reddit growth. | Those claims are in the original entry thesis; today's analyst instead says 1.5x volume. The manager copied the misattribution into its sale. | Skeptic critiques today's file without entry-day thesis statistics. Exit checker sees explicitly separated current and historical evidence. |
| TTWO critic argues about which day the launch happened. | Stored headlines and earlier notes conflict on relative wording. Today's decline is real in the dossier, but launch-day ambiguity alone does not establish failed sales or revenue. | Use calendar dates and verify the proposed sale against the actual dossier, not just the skeptic verdict. The sale can still be justified by supported risk evidence. |
| WMT case is rejected because its Q3 2027 label is supposedly in the future. | Fiscal-year labels are not calendar publication dates. The headline publication timestamp is in the decision day's file. | Explicit fiscal/calendar distinction in skeptic and exit-check instructions. This does not automatically qualify WMT for purchase. |
| Local 9B manager makes all four exit decisions. | Both hosted holdings attempts consumed 2,500 reasoning tokens and returned empty content. The fallback answered in about 754 seconds. | Explicitly disable thinking and request nonempty content for Nemotron's constrained JSON calls; keep schema validation and fallback for other failures. |

[NVIDIA's model guidance](https://docs.dynamo.nvidia.com/dynamo/v1.5.0/recipes/nemotron-3-ultra)
documents the thinking switch, nonempty-content control, and limitations with
reasoning plus constrained output. The change targets the observed empty-answer
failure, not an assumed profitability gain.

## Exit checks

Every ordinary proposed sale now receives a separate fact check using the dated
raw dossier, current analyst and original entry evidence. It does not receive the
skeptic's narrative as authority. An approved sale must quote an actual source;
invalid quotes, rejected checks and failed checks retain the holding. Mandatory
position-count trims remain possible. Source matching cannot prove that a model's
interpretation is right, so all checks and transcripts remain inspectable.

Old pending sales are reviewed before settlement. A manual `audit-exits` command
and the **Review pending exits** workflow can perform the same check without filling
orders or researching a new market day. The workflow archives the original ledger,
records review results and transcripts, then publishes the revised queue. Original
reports and journal decisions are not replaced. Signal-time prices and news dates
bound the review; subsequent returns are not used to choose the result.
