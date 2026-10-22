-- Jivvy Live: the cloud copy of each church's library, for the church computer's sync engine
-- (apps/daemon/src/sync.rs; its MemoryCloud follows the same rules). For the owner to apply
-- to the Supabase project (SQL editor or `supabase db push`); nothing in the repo applies it.
-- The church computer stays the master copy: this is a synced copy, never Sunday's source of truth.
--
-- One row per field of each document (`title`, `deleted`, `body.<key>`), stamped with a hybrid
-- logical clock (`hlc`: 13-digit ms, 6-digit counter, node id; fixed width, compared bytewise
-- with collation "C", so text order is time order). A push says which clock it built on
-- (`base`); a field is changed only if that is still the clock here and the new one is later
-- (compare and set). Otherwise the row here is handed back, and the church computer merges it
-- (keeping both versions of a song's words) before trying again, so no edit is lost unseen.
-- Every write takes the next `seq`; the church computer pulls `seq > cursor` in order. Writes
-- are serialized per church (advisory lock), so a church's seq values commit in order and a
-- pull never skips one.
--
-- Who's who: each church signs in as a Supabase Auth user whose app_metadata.church_id is the
-- `church` of its license key (set by live.jivvy.org with the service role, never by the user).

create table if not exists public.sync_fields (
  church_id text not null,
  kind text not null check (kind ~ '^[a-z][a-z0-9-]{0,31}$'),
  doc_id text not null check (doc_id ~ '^[A-Za-z0-9_-]{1,64}$'),
  field text not null check (char_length(field) between 1 and 200),
  value jsonb not null,
  hlc text collate "C" not null check (hlc ~ '^[0-9]{13}-[0-9]{6}-[A-Za-z0-9_-]{1,16}$'),
  seq bigint not null,
  updated_at timestamptz not null default now(),
  primary key (church_id, kind, doc_id, field)
);

create sequence if not exists public.sync_fields_seq;
create index if not exists sync_fields_church_seq on public.sync_fields (church_id, seq);

-- The church a signed-in church computer belongs to; null for anyone else.
create or replace function public.sync_church() returns text
language sql stable as $$
  select nullif(auth.jwt() -> 'app_metadata' ->> 'church_id', '')
$$;

alter table public.sync_fields enable row level security;

-- Each church reads only its own rows. Writes go through sync_push only.
drop policy if exists sync_fields_read_own on public.sync_fields;
create policy sync_fields_read_own on public.sync_fields
  for select to authenticated
  using (church_id = public.sync_church());

revoke all on public.sync_fields from public, anon, authenticated;
grant select on public.sync_fields to authenticated;
revoke all on sequence public.sync_fields_seq from public, anon, authenticated;

-- Saves a batch of field changes for the caller's church: [{kind, id, field, value, hlc, base?}].
-- Returns the rows it refused (the cloud has a different or later clock), as
-- [{kind, id, field, value, hlc}], for the church computer to merge and send again.
create or replace function public.sync_push(changes jsonb) returns jsonb
language plpgsql security definer set search_path = public as $$
declare
  church text := public.sync_church();
  c jsonb;
  have public.sync_fields;
  refused jsonb := '[]'::jsonb;
  new_hlc text;
begin
  if church is null then
    raise exception 'not a church computer' using errcode = '42501';
  end if;
  if jsonb_typeof(changes) <> 'array' or jsonb_array_length(changes) > 500 then
    raise exception 'changes must be an array of at most 500' using errcode = '22023';
  end if;
  -- One writer per church at a time, so its seq values commit in order.
  perform pg_advisory_xact_lock(hashtextextended('jivvy-sync:' || church, 0));
  for c in select * from jsonb_array_elements(changes) loop
    new_hlc := c ->> 'hlc';
    if new_hlc is null or new_hlc !~ '^[0-9]{13}-[0-9]{6}-[A-Za-z0-9_-]{1,16}$' then
      raise exception 'hlc must be 13 digits, 6 digits and a node id' using errcode = '22023';
    end if;
    -- A body field at most the daemon's whole-document limit (512 KB).
    if pg_column_size(coalesce(c -> 'value', 'null'::jsonb)) > 524288 then
      raise exception 'a value can be at most 512 KB' using errcode = '22023';
    end if;
    select * into have from public.sync_fields
      where church_id = church and kind = c ->> 'kind' and doc_id = c ->> 'id' and field = c ->> 'field'
      for update;
    if found and (have.hlc is distinct from (c ->> 'base') collate "C" or new_hlc collate "C" <= have.hlc) then
      refused := refused || jsonb_build_array(jsonb_build_object(
        'kind', have.kind, 'id', have.doc_id, 'field', have.field, 'value', have.value, 'hlc', have.hlc));
      continue;
    end if;
    insert into public.sync_fields as f (church_id, kind, doc_id, field, value, hlc, seq)
    values (church, c ->> 'kind', c ->> 'id', c ->> 'field', coalesce(c -> 'value', 'null'::jsonb), new_hlc,
            nextval('public.sync_fields_seq'))
    on conflict (church_id, kind, doc_id, field) do update
      set value = excluded.value, hlc = excluded.hlc, seq = excluded.seq, updated_at = now();
  end loop;
  return refused;
end
$$;

revoke all on function public.sync_push(jsonb) from public, anon;
grant execute on function public.sync_push(jsonb) to authenticated;
