export const legacyCommands = new Set(['register', 'signup', 'equipment', 'documents', 'handout', 'docs', 'link-account']);

const memberHelp = [
  '**pdfsign commands**',
  '`!help` - Show this menu.',
  '`!register` - Open private registration for your name and ZIP code.',
  '`!signup [event]` - Check your waiver or get a signing link.',
  '`!equipment` - Open your equipment form.',
  '`!documents` - List your documents and their links.',
];

const staffHelp = [
  '',
  '**Staff commands**',
  '`!handout @member [items]` - DM an equipment form.',
  '`!docs status @member` - Check a member\'s documents.',
  '`!docs send @member <template-slug>` - DM a form for a template.',
  '`!link-account <old-user-id> @new-member` - Move records to a replacement account.',
];

export function commandHelp(staff: boolean, intro?: string) {
  const lines = intro ? [intro, '', ...memberHelp] : [...memberHelp];
  if (staff) lines.push(...staffHelp);
  return lines.join('\n');
}
