import type { CurrentUser } from './api';

export const session = $state({
  user: null as CurrentUser | null, exportDir: '', publicUrl: '', version: '', eventsEnabled: false,
});
export const isAdmin = () => session.user?.role === 'admin';
export const isStaff = () => isAdmin() || session.user?.role === 'staff';
