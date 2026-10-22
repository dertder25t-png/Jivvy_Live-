<script lang="ts">
  import type { Ack, Command } from "@jivvy/protocol";

  /**
   * Alerts on this device: the church computer pushes "the stream is down", "no sound" and
   * platform alerts here by web push, even with no Jivvy screen open. The browser subscribes
   * through the app's service worker and hands the subscription to the church computer.
   */
  let { ready, send }: { ready: boolean; send: (command: Command) => Promise<Ack> | undefined } = $props();

  const supported =
    typeof navigator !== "undefined" && "serviceWorker" in navigator && typeof window !== "undefined" && "PushManager" in window;
  let on = $state(false);
  let busy = $state(false);
  let note = $state("");

  /** base64url (the protocol's keys) to bytes, for applicationServerKey. */
  function bytes(b64url: string): Uint8Array<ArrayBuffer> {
    const s = atob(b64url.replace(/-/g, "+").replace(/_/g, "/"));
    const out = new Uint8Array(new ArrayBuffer(s.length));
    for (let i = 0; i < s.length; i++) out[i] = s.charCodeAt(i);
    return out;
  }
  const unpadded = (s: string | undefined) => (s ?? "").replace(/=+$/, "");

  async function turnOn() {
    busy = true;
    note = "";
    try {
      if ((await Notification.requestPermission()) !== "granted") {
        note = "Notifications are blocked for this site. Allow them in the browser's settings, then try again.";
        return;
      }
      const key = await send({ type: "push.key" });
      if (!key?.ok || !key.pushKey) {
        note = key && !key.ok ? key.message : "Not connected.";
        return;
      }
      const reg = await navigator.serviceWorker.ready;
      let sub = await reg.pushManager.getSubscription();
      // One made for another church computer's key can't be reused.
      if (sub) await sub.unsubscribe();
      sub = await reg.pushManager.subscribe({ userVisibleOnly: true, applicationServerKey: bytes(key.pushKey) });
      const json = sub.toJSON();
      const ack = await send({
        type: "push.subscribe",
        endpoint: sub.endpoint,
        p256dh: unpadded(json.keys?.p256dh),
        auth: unpadded(json.keys?.auth),
      });
      if (ack?.ok) on = true;
      else note = ack?.message ?? "Not connected.";
    } catch (e) {
      note = `This browser couldn't turn on alerts (${e instanceof Error ? e.message : String(e)}). On an iPhone, add Jivvy to the home screen first.`;
    } finally {
      busy = false;
    }
  }

  async function test() {
    note = "";
    const ack = await send({ type: "push.test" });
    note = ack?.ok ? "Test alert sent. It should arrive in a few seconds." : (ack?.message ?? "Not connected.");
  }
</script>

<section class="alerts" aria-label="Alerts on this device">
  <div class="head">
    <div>
      <h2>Alerts on this device</h2>
      <p class="hint">Get a notification here if the stream goes down or there's no sound, even with Jivvy closed.</p>
    </div>
    <div class="buttons">
      <button class="primary" onclick={turnOn} disabled={!ready || busy || !supported}>
        {busy ? "Turning on…" : on ? "Alerts on" : "Turn on alerts"}
      </button>
      <button onclick={test} disabled={!ready || !on}>Send test alert</button>
    </div>
  </div>
  {#if !supported}<p class="note">This browser can't get alerts. On an iPhone, add Jivvy to the home screen first.</p>{/if}
  {#if note}<p class="note" role="status">{note}</p>{/if}
</section>

<style>
  .alerts {
    max-width: 1100px;
    margin: 16px auto 32px;
    padding: 0 20px;
  }
  .head {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    justify-content: space-between;
    gap: 8px 16px;
  }
  .buttons {
    display: flex;
    gap: 8px;
  }
  h2 {
    font-size: 18px;
    margin: 0;
    color: var(--navy);
  }
  .hint {
    color: var(--muted);
    font-size: 13px;
    margin: 4px 0;
  }
  .note {
    font-size: 14px;
    margin: 10px 0 0;
    padding: 8px 12px;
    border-radius: 6px;
    background: var(--card);
    border: 1px solid var(--line);
  }
  button {
    border: 1px solid var(--line);
    background: var(--card);
    border-radius: 6px;
    padding: 10px 18px;
    cursor: pointer;
  }
  button:disabled {
    opacity: 0.45;
    cursor: default;
  }
  button.primary {
    background: var(--blue);
    border-color: var(--blue);
    color: #fff;
    font-weight: 600;
  }
</style>
