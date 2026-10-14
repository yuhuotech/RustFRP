import type { Context, Page, Section } from '../app/context';

export async function loadPage(context: Context, section: Section, id?: string): Promise<Page> {
  if (section === 'overview') return (await import('./overview')).overviewPage(context);
  if (section === 'local') return (await import('./local')).localPage(context);
  if (section === 'nodes') return (await import('./nodes')).nodesPage(context, id);
  if (section === 'clients') return (await import('./clients')).clientsPage(context, id);
  if (section === 'tunnels') return (await import('./tunnels')).tunnelsPage(context, id);
  if (section === 'audit') return (await import('./audit')).auditPage(context);
  return (await import('./settings')).settingsPage(context, id);
}
