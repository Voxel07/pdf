import type { CurrentUser } from './api';

export const session = $state<{ user: CurrentUser | null; exportDir: string; publicUrl: string; version: string }>({ user: null, exportDir: '', publicUrl: '', version: '' });
export const isAdmin = () => session.user?.role === 'admin';
