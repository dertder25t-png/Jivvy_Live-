import { countUpload } from "../../src/lib/uploadTest";

const json = (body: unknown, status = 200) =>
  new Response(JSON.stringify(body), { status, headers: { "content-type": "application/json", "cache-control": "no-store" } });

// The church computer times the upload itself; this only counts what arrived.
export const onRequestPost: PagesFunction = async ({ request }) => {
  const r = await countUpload(request.body);
  return r.ok ? json({ ok: true, bytes: r.bytes }) : json({ ok: false, bytes: r.bytes, error: r.error }, 413);
};
