# Mission Cloud

> **Mission** (the [open-source parser](../README.md)) answers *"given this HTML and this selector,
> what's the data?"* — deterministically, in microseconds, on your machine.
>
> **Mission Cloud** answers the harder question: *"keep that extraction working, at scale, while the
> web changes underneath it."*

Mission Cloud is the managed platform built **on top of** the free parser. Same engine you install
from this repo — wrapped in a self-healing brain, a high-throughput transport, and hosted
infrastructure. It's a separate commercial product; the parser stays free and open forever.

---

## The problem the parser alone doesn't solve

The parser is the easy, deterministic half. The expensive half is everything around it:

- **Selectors rot.** A site ships a redesign and `.price` becomes `.product__price`. Your job breaks
  silently at 3am.
- **Pages flake.** Transient failures, partial responses, rate limits — one bad response shouldn't
  take down a pipeline.
- **Scale is operational.** Thousands of pages a minute, retries, backpressure, monitoring — that's
  infrastructure, not a parser.

Mission Cloud is the layer that turns "a parser that works" into "extraction that *stays* working."

## What Mission Cloud adds

### 🧠 Self-healing extraction
A deterministic policy engine sits between your job and the parser. When a selector stops matching,
it doesn't just fail — it **retries with fallback selectors**, **escalates to a broader strategy**
when a whole approach stops working, and **opens a circuit breaker** when something is genuinely down
(so a broken target degrades gracefully instead of hammering it). You define the intent; the platform
keeps it alive as the page drifts.

### ⚡ High-throughput binary transport
For pipelines moving millions of small extraction jobs, JSON-over-HTTP is overhead. Mission Cloud
speaks a compact, integrity-checked (CRC) binary framing with zero-copy decoding — less work per
message, with corruption caught at the wire, so a stream of jobs runs as a continuous, low-latency
machine.

### ☁️ Managed & distributed
Run extraction at scale without owning the operational burden: the platform distributes work,
mediates the slicer-and-brain feedback loop, and stays up. Hosted, monitored, and observable — you
watch outcomes, not infrastructure.

### 📊 Outcomes you can see
Which selectors are drifting, which strategies are carrying the load, where the circuit opened and
why — the platform surfaces the health of your extraction, not just its output.

## Free vs. Mission Cloud

| | **Mission** (free, open source) | **Mission Cloud** (managed) |
| --- | --- | --- |
| HTML parse · CSS query · render | ✅ | ✅ |
| Zero-dependency, self-hosted, MIT/Apache | ✅ | ✅ |
| MCP server (`mission-mcp`) | ✅ | ✅ |
| Auto-retry with fallback selectors | — | ✅ |
| Strategy escalation + circuit breaking | — | ✅ |
| High-throughput binary job transport | — | ✅ |
| Managed, distributed, monitored at scale | — | ✅ |
| Extraction-health observability | — | ✅ |
| **Price** | **free, forever** | commercial · [join the waitlist](#get-early-access) |

The free parser is the **genuine core** of the platform — not a limited demo. Everything above the
free line is yours with no strings; Mission Cloud is what you reach for when extraction becomes
something you have to operate.

---

## <a id="get-early-access"></a>☁️ Get early access

Mission Cloud is opening to early-access partners now.

> ### → [Request early access](https://github.com/MerlijnW70/mission/issues/new?title=Mission%20Cloud%20early%20access)

<sub>*(Interim signup via GitHub. A dedicated waitlist form is coming — this link will be updated.)*</sub>

Prefer to start free? [Install the parser](../README.md) — it's the same engine, and it's yours to keep.
