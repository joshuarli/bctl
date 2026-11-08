Yes. Kernel has shipped a concrete follow-up to that September 21 teaser, and its open-source repositories reveal several clues about the architecture.

The original post from Catherine Jue said that website access—not model intelligence—had become the biggest obstacle to production browser agents, and teased a solution for accessing any website.&#x20;

[image](https://www.google.com/s2/favicons?domain=https://nitter.cf\&sz=32)

teapawt



On October 8, 2026, she announced Config Registry: a self-improving API that determines the appropriate Kernel browser configuration for a given website.&#x20;

[image](https://www.google.com/s2/favicons?domain=https://nitter.cf\&sz=32)

teapawt



That's an important distinction. Rather than expecting one universal stealth configuration to work everywhere, Kernel appears to be moving toward site-specific access strategies, backed by accumulated operational knowledge.

The interesting question is how much of that intelligence is open source, how configurations are selected, and whether their approach actually generalizes.

## 1. What they've actually achieved

Kernel's [October 7 engineering/product announcement](https://www.kernel.sh/blog/config-registry?utm_source=chatgpt.com) describes a fairly concrete system:

Architecture reconstructed from Kernel's published description; not a diagram of its unpublished internal implementation.

On a cache miss, their service launches multiple browser/proxy configurations against the live target, classifies the results with an LLM, and repeats successful configurations to establish reliability. It can also attempt an actual workflow rather than merely loading the homepage. Results include success rates, trial counts, and recency.&#x20;

[image](https://www.google.com/s2/favicons?domain=https://www.kernel.sh\&sz=32)

Kernel Blog+1



Kernel's reported pre-launch result

# 96.1%

of previously inaccessible sites became reachable using recommended configurations

That is a substantial claim, but the distinction matters: this was Kernel's own testing across hundreds of previously blocked sites. It demonstrates reported site reachability, not a 96.1% autonomous task-completion rate, and I found no independently published dataset establishing the result.&#x20;

[image](https://www.google.com/s2/favicons?domain=https://www.kernel.sh\&sz=32)

Kernel Blog



## 2. What I found in `github.com/kernel`

The most revealing repositories are not the SDKs themselves, but their browser runtime and feedback instrumentation.

| Repository / source                                                                                                                                  | What it reveals                                                                                |
| ---------------------------------------------------------------------------------------------------------------------------------------------------- | ---------------------------------------------------------------------------------------------- |
| [kernel-images](https://github.com/kernel/kernel-images?utm_source=chatgpt.com)                                                                      | Actual Chromium runtime, browser flags, input handling, session control                        |
| [MCP feedback.ts](https://github.com/kernel/kernel-mcp-server/blob/main/src/lib/mcp/tools/feedback.ts?utm_source=chatgpt.com)                        | Structured reporting of website compatibility, challenges, and configuration outcomes          |
| [Config Registry types](https://github.com/kernel/kernel-node-sdk/blob/main/src/resources/config-registry/config-registry.ts?utm_source=chatgpt.com) | The concrete configuration space, evidence model, and analysis lifecycle                       |
| [hypeman](https://github.com/kernel/hypeman?utm_source=chatgpt.com)                                                                                  | VM lifecycle, Firecracker/Cloud Hypervisor integration, snapshot-based standby and restoration |

### The browser is less exotic than I expected

The published `kernel-images` Dockerfiles install Chrome for Testing, rather than visibly compiling a bespoke Chromium fork.

The runtime also has several interesting details:

- `--disable-blink-features=AutomationControlled` and curated Chromium flags.
- A much more complete Ubuntu font collection to avoid inconsistent browser fingerprints.
- Patchright as the default engine for a persistent, in-VM Playwright executor.
- OS-level mouse and keyboard control, with randomized Bézier mouse trajectories and typing timing.
- Browser execution through a warm daemon, reducing repeated initialization and external round trips.

You can inspect the actual implementations in [chromium.go](https://github.com/kernel/kernel-images/blob/main/server/cmd/wrapper/chromium.go?utm_source=chatgpt.com), [playwright-daemon.ts](https://github.com/kernel/kernel-images/blob/main/server/runtime/playwright-daemon.ts?utm_source=chatgpt.com), and [mousetrajectory.go](https://github.com/kernel/kernel-images/blob/main/server/lib/mousetrajectory/mousetrajectory.go?utm_source=chatgpt.com).

There's a qualification here: Kernel discusses engine-level Chromium patching in its research, but I could not establish from these public Dockerfiles that its currently published browser images ship a custom-patched Chromium binary. Private production modifications may exist.

### The feedback mechanism is particularly interesting

In `kernel-mcp-server`, there's a structured `submit_feedback` tool.

It accepts website compatibility observations such as `passed`, `challenged`, `blocked`, and `degraded`, along with browser version, proxy type, region, reproducibility, and applied Config Registry settings.

It also captures the recommendation's previous evidence: sample size, success rate, and verification timestamp.

This is precisely the kind of instrumentation needed for a continuously improving compatibility system. It provides a way to correlate the recommended configuration with subsequent real-world results.

What isn't public: the actual Config Registry ranking, experiment scheduling, evidence aggregation, and LLM decision-engine implementation. The GitHub code proves the API and feedback contract, not that every submitted observation automatically changes future recommendations.

## 3. Their most important technical advantages

The [Kernel anti-bot research](https://www.kernel.sh/ai-library/bypass-bot-detection-ai-agents-2026?utm_source=chatgpt.com) is surprisingly candid about which techniques work and which do not.

Network identity appears to be the biggest lever. In Kernel's reported A/B experiments, changing from datacenter egress to suitable ISP addresses improved success rates on some Cloudflare-protected targets from approximately 6% into the 60% range. Their research also found that shared residential IPs aren't invariably better than static ISP IPs.&#x20;

[image](https://www.google.com/s2/favicons?domain=https://dev.kernel.sh\&sz=32)

dev.kernel.sh



Automation-protocol behavior matters independently of browser appearance. Their [Kasada reverse-engineering investigation](https://www.kernel.sh/blog/detection?utm_source=chatgpt.com) documents how Playwright's CDP initialization affected V8 inspector state in ways a detector could observe. Their experiments showed that using the same browser through a thinner CDP client could produce different results. That's a credible explanation for why a browser with otherwise convincing fingerprints still gets blocked.&#x20;

[image](https://www.google.com/s2/favicons?domain=https://www.kernel.sh\&sz=32)

Kernel Blog



The best strategy is often conditional. Their findings include cases where a CAPTCHA-solving extension actually reduced the pass rate, where a warmed browser profile dramatically improved access, and other cases where a fresh profile worked better. This makes experimental configuration selection much more valuable than a universal stealth preset.&#x20;

[image](https://www.google.com/s2/favicons?domain=https://dev.kernel.sh\&sz=32)

dev.kernel.sh



### A separate architectural improvement: host-side proxying

Their [September 1 proxy engineering write-up](https://www.kernel.sh/blog/proxy?utm_source=chatgpt.com) describes moving proxy management out of individual browser VMs into an always-on, host-side Go service.

The browser can sleep or resume without bringing stale proxy connections back with it. Proxy configurations can be changed during a session, and network failures can be attributed more precisely.

Kernel reports 92% faster wakeups and much better post-standby reliability from this change.&#x20;

[image](https://www.google.com/s2/favicons?domain=https://www.kernel.sh\&sz=32)

Kernel Blog



This is important because the Config Registry needs to experiment with different network paths without unnecessarily rebuilding browser state.

It's also worth distinguishing from Hypeman's microVM snapshots. Fast VM restoration makes the experiments inexpensive; the host-side proxy architecture and Config Registry are what make access decisions adaptable.

## 4. What's genuinely new versus proprietary

My assessment of the public evidence:

| Capability                                        | Visibility                           |
| ------------------------------------------------- | ------------------------------------ |
| Browser runtime and automation controls           | Open-source implementation           |
| VM lifecycle and snapshots                        | Open-source implementation           |
| Site-specific configuration API                   | Public contract and documentation    |
| Experiment orchestration and LLM classifier       | Described, implementation not public |
| Shared cross-site knowledge and ranking           | Described, internal logic not public |
| Production proxy service and routing intelligence | Partially described                  |
| Independent proof of broad website/task success   | Not established                      |

There is also a longer-term strategy that differs fundamentally from stealth: Web Bot Auth, where participating websites can recognize authorized agents cryptographically. That is potentially much more sustainable, but it works only where the destination supports and permits that identity.&#x20;

[image](https://www.google.com/s2/favicons?domain=https://dev.kernel.sh\&sz=32)

dev.kernel.sh



And in the October 16 changelog, Kernel confirmed Config Registry had reached general availability on Hobbyist plans and above, alongside residential multi-provider failover improvements.&#x20;

[image](https://www.google.com/s2/favicons?domain=https://www.kernel.sh\&sz=32)

Kernel



## 5. What I'd adopt for your projects

I would borrow the adaptive access control plane, not reproduce Kernel's complete browser infrastructure.

For `usr`, the useful addition would be a small typed `site-access` subsystem: normalized target and workflow intent, a registry of observed configurations, explicit outcome classifications, and a bounded experimental resolver when access fails. Maintain confidence, sample counts, observation dates, and stale-evidence invalidation. Most importantly, measure task completion, not just successful page loads.

For Laputa, Hypeman's snapshot/restore design is worth studying separately. Kernel's headful browser images use Ubuntu, X11, PulseAudio, and other components that would be a poor match for your minimalist userspace. The registry and evaluation architecture are much more portable than that runtime.

One operational caveat: Kernel also disclosed a [November 4 security incident](https://www.kernel.sh/blog/public-notice-of-security-incident-nov-4-2026?utm_source=chatgpt.com) involving compromised credentials and access to some infrastructure. They report 0.28% of customers were affected and say containment and credential rotation were completed. That matters when evaluating it as a host for authenticated browser sessions.&#x20;

[image](https://www.google.com/s2/favicons?domain=https://www.kernel.sh\&sz=32)

Kernel Blog



Bottom line: I don't see evidence of one miraculous anti-bot breakthrough. I see an increasingly sophisticated system for experimentally discovering what works, caching that knowledge, and applying it economically across workloads.

That's arguably more interesting. The browser runtime itself is largely reproducible; the accumulated compatibility evidence and decision engine look like the real potential moat.&#x20;