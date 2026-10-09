import { describe, expect, it } from "vitest";
import { dispatch, makeEnvelope, type Command } from "@jivvy/protocol";
import shared from "../../protocol/fixtures/scenarios.json";
import { SimDaemon } from "../src/index";

// The same scenarios run against the real daemon (apps/daemon/tests/chaos.rs).
type Step = { send: Command; expect: Record<string, unknown> } | { crash: true };
const scenarios = shared.scenarios as Array<{ name: string; slides: number; steps: Step[] }>;

describe("shared scenarios (same as the real daemon)", () => {
  it("has some", () => expect(scenarios.length).toBeGreaterThanOrEqual(5));

  for (const s of scenarios) {
    it(s.name, async () => {
      const timers: Array<() => void> = [];
      const d = new SimDaemon({ slideCount: s.slides, setTimer: (fn) => timers.push(fn) });
      for (const [i, step] of s.steps.entries()) {
        if ("crash" in step) {
          d.crash();
          timers.splice(0).forEach((f) => f()); // restarted from its snapshot
          continue;
        }
        const ack = await dispatch(d, makeEnvelope(step.send));
        expect(ack, `step ${i}: ${JSON.stringify(step.send)}`).toMatchObject(step.expect);
      }
    });
  }
});
