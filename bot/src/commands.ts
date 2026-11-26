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

export function commandHelp(staff: boolean, signing: boolean, events: boolean, intro?: string) {
  const commands = signing ? memberHelp : memberHelp.slice(0, 2);
  const lines = intro ? [intro, '', ...commands] : [...commands];
  if (staff && signing) lines.push(...staffHelp);
  if (events) lines.push(
    '',
    '**Events · Authentik login**',
    '!event list · Browse events.',
    '!event create · Schedule an event.',
    '!event show <id> · View and RSVP.',
    '!event edit <id> · Update an event.',
    '!event cancel <id> · Review and cancel an event.',
  );
  return lines.join('\n');
}
