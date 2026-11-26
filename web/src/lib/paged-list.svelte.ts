import { admin } from './api';

export const pageSize = 50;

export function pagedList<T extends { id: string }>(cursorKey: (item: T) => string) {
  let items = $state<T[]>([]);
  let loading = $state(true);
  let error = $state('');
  let page = $state(1);
  let generation = 0;

  async function load(path: string, resetPage = true) {
    const current = ++generation;
    if (resetPage) {
      page = 1;
      items = [];
    }
    loading = true;
    error = '';
    const url = new URL(path, location.origin);
    url.searchParams.set('limit', String(pageSize));
    const seen = new Set<string>();
    let buffered: T[] = [];
    let published = false;
    try {
      while (current === generation) {
        const batch = await admin.get<T[]>(url.pathname + url.search);
        if (current !== generation) return;
        for (const item of batch) {
          if (!seen.has(item.id)) buffered.push(item);
          seen.add(item.id);
        }
        const done = batch.length < pageSize;
        // Keep the current page visible while refreshing after a change.
        if (published) items.push(...buffered);
        else if (done || buffered.length >= page * pageSize) {
          items = buffered;
          published = true;
        }
        if (published) buffered = [];
        if (done) {
          page = Math.min(page, Math.max(1, Math.ceil(items.length / pageSize)));
          break;
        }
        const last = batch[batch.length - 1];
        url.searchParams.set('after_key', cursorKey(last));
        url.searchParams.set('after_id', last.id);
        await new Promise((resolve) => setTimeout(resolve, 0));
      }
    } catch (e) {
      if (current === generation) error = e instanceof Error ? e.message : String(e);
    } finally {
      if (current === generation) loading = false;
    }
  }

  return {
    get items() { return items; },
    get loading() { return loading; },
    get error() { return error; },
    get page() { return page; },
    set page(value: number) { page = value; },
    load,
    cancel() { generation++; },
  };
}
