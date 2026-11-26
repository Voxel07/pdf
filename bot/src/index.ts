import {
  ActionRowBuilder,
  ButtonBuilder,
  ButtonStyle,
  ApplicationCommandType,
  Client,
  EmbedBuilder,
  Events,
  GatewayIntentBits,
  MessageFlags,
  ModalBuilder,
  Partials,
  TextInputBuilder,
  TextInputStyle,
  type Interaction,
  type Message,
  type MessageCreateOptions,
  type User,
} from 'discord.js';
import { api, ApiError, type Submission } from './api';
import { commandHelp, legacyCommands } from './commands';
import { config } from './config';

const client = new Client({
  intents: [GatewayIntentBits.Guilds, GatewayIntentBits.GuildMessages, GatewayIntentBits.MessageContent, GatewayIntentBits.GuildScheduledEvents, GatewayIntentBits.DirectMessages],
  partials: [Partials.Channel, Partials.GuildScheduledEvent, Partials.User],
});

// "purpose" encodes what to deliver after registration: event | equip | tpl:<slug>
type Purpose = string;
const templateFor = (p: Purpose) => (p === 'event' ? config.eventTemplate : p === 'equip' ? config.equipmentTemplate : p.slice(4));

const fmt = (d?: string | null) => (d ? `<t:${Math.floor(new Date(d).getTime() / 1000)}:D>` : 'forever');

function linkRow(url: string, label = 'Open document') {
  return new ActionRowBuilder<ButtonBuilder>().addComponents(new ButtonBuilder().setStyle(ButtonStyle.Link).setURL(url).setLabel(label));
}

function registerRow(purpose: Purpose) {
  return new ActionRowBuilder<ButtonBuilder>().addComponents(
    new ButtonBuilder().setCustomId(`regbtn:${purpose}`).setStyle(ButtonStyle.Primary).setLabel('Register my name'),
  );
}

function registerModal(purpose: Purpose) {
  return new ModalBuilder()
    .setCustomId(`reg:${purpose}`)
    .setTitle('Register')
    .addComponents(
      new ActionRowBuilder<TextInputBuilder>().addComponents(
        new TextInputBuilder()
          .setCustomId('name')
          .setLabel('Your full legal name')
          .setPlaceholder('First and last name')
          .setStyle(TextInputStyle.Short)
          .setMinLength(3)
          .setMaxLength(120)
          .setRequired(true),
      ),
      new ActionRowBuilder<TextInputBuilder>().addComponents(
        new TextInputBuilder()
          .setCustomId('zip')
          .setLabel('Zip code')
          .setPlaceholder('Helps to tell people with the same name apart')
          .setStyle(TextInputStyle.Short)
          .setMinLength(3)
          .setMaxLength(10)
          .setRequired(true),
      ),
    );
}

function docEmbed(s: Submission, intro: string) {
  return new EmbedBuilder()
    .setTitle(s.template_name)
    .setDescription(
      `${intro}\n\nWhen you open the link you'll be asked for your **full name** as registered. The link is personal — don't share it.`,
    )
    .setColor(s.status === 'completed' ? 0x15803d : 0x2f5bd3)
    .addFields({ name: 'Status', value: s.valid ? `valid until ${fmt(s.valid_until)}` : s.status, inline: true });
}

async function notifyStaff(text: string) {
  if (!config.notifyChannelId) return;
  try {
    const ch = await client.channels.fetch(config.notifyChannelId);
    if (ch?.isSendable()) await ch.send(text);
  } catch (e) {
    console.warn('notify failed', e);
  }
}

async function dm(user: User, payload: Parameters<User['send']>[0]): Promise<boolean> {
  try {
    await user.send(payload);
    return true;
  } catch {
    return false;
  }
}

type Outcome = { kind: 'valid'; sub: Submission } | { kind: 'need_name' } | { kind: 'link'; sub: Submission };

/** Checks the user's document for a purpose and returns a link if action is needed. */
async function resolve(user: User, purpose: Purpose): Promise<Outcome> {
  const template = templateFor(purpose);
  const status = await api.status(user.id, template);
  if (!status.registered) return { kind: 'need_name' };
  if (purpose === 'event' && status.valid && status.submission) return { kind: 'valid', sub: status.submission };
  const sub = await api.link(template, user.id, user.username);
  return { kind: 'link', sub };
}

function introFor(purpose: Purpose, sub: Submission, extra?: string) {
  if (purpose === 'event') return 'To take part in events you need a signed waiver (valid for a limited time). Please sign it here:';
  if (purpose === 'equip')
    return (extra ? `You received: **${extra}**\n` : '') + 'Please open your equipment form and add the items you received, then sign the new entries.';
  return sub.status === 'pending' ? 'Please fill in and sign this document:' : 'You can view or continue your document here:';
}

// -----------------------------------------------------------------------------------------

function isStaff(message: Message) {
  const member = message.member;
  if (!member) return false;
  if (config.adminRoleIds.length) return member.roles.cache.some((role) => config.adminRoleIds.includes(role.id));
  return member.permissions.has('ManageGuild');
}

async function privateReply(message: Message, payload: string | MessageCreateOptions) {
  const options = typeof payload === 'string' ? { content: payload } : payload;
  options.allowedMentions = { parse: [], repliedUser: false };
  if (!message.inGuild()) return message.reply(options);
  if (!(await dm(message.author, options))) {
    return message.reply({ content: 'I could not DM you. Enable direct messages from server members to receive private replies.', allowedMentions: { repliedUser: false } });
  }
}

async function handleCommand(message: Message, command: string, args: string) {
  const user = message.author;
  const staff = isStaff(message);
  const reply = (payload: string | MessageCreateOptions) => privateReply(message, payload);
  const help = (intro?: string) => message.reply({ content: commandHelp(staff, intro), allowedMentions: { repliedUser: false } });
  const askToRegister = (purpose: Purpose) =>
    reply({ content: 'Register your name and ZIP code to continue.', components: [registerRow(purpose)] });
  const [first, second, ...rest] = args.split(/\s+/);

  async function findMember(input: string | undefined) {
    const match = /^(?:<@!?(\d+)>|(\d+))$/.exec(input ?? '');
    if (!message.inGuild() || !match) return undefined;
    try {
      return (await message.guild.members.fetch(match[1] ?? match[2])).user;
    } catch (error) {
      if (error && typeof error === 'object' && 'code' in error && (error.code === 10007 || error.code === 10013)) return undefined;
      throw error;
    }
  }

  switch (command) {
    case 'help':
      return help();
    case 'register':
      return args ? help('Use !register without arguments to enter your details privately.') : askToRegister('none');

    case 'signup':
    case 'equipment': {
      if (command === 'equipment' && args) return help('Usage: !equipment');
      const purpose = command === 'signup' ? 'event' : 'equip';
      const out = await resolve(user, purpose);
      if (out.kind === 'need_name') return askToRegister(purpose);
      if (out.kind === 'valid')
        return reply({ content: `✅ You're all set${args ? ` for **${args}**` : ''} — your waiver is valid until ${fmt(out.sub.valid_until)}.` });
      return reply({ embeds: [docEmbed(out.sub, introFor(purpose, out.sub))], components: [linkRow(out.sub.url)] });
    }

    case 'documents': {
      if (args) return help('Usage: !documents');
      let view;
      try {
        view = await api.person(user.id);
      } catch (e) {
        if (e instanceof ApiError && e.status === 404) return askToRegister('none');
        throw e;
      }
      const subs = view.submissions.filter((s) => s.status !== 'revoked');
      if (!subs.length) return reply({ content: 'You have no documents yet.' });
      const lines = subs.map((s) => `• **${s.template_name}** — ${s.valid ? `✅ valid until ${fmt(s.valid_until)}` : s.status === 'pending' ? '✍️ waiting for your signature' : `⚠️ ${s.status}`}`);
      const rows = subs.slice(0, 5).map((s) => linkRow(s.url, s.template_name.slice(0, 70)));
      return reply({ content: `Registered as **${view.person.name}**\n${lines.join('\n')}`, components: rows });
    }

    case 'handout': {
      if (!staff) return message.reply('Staff only. Run this command in the server.');
      const target = await findMember(first);
      if (!target) return help('Usage: !handout @member [items]');
      const items = args.slice(first.length).trim();
      const out = await resolve(target, 'equip');
      if (out.kind === 'need_name') {
        const ok = await dm(target, {
          content: `You received equipment${items ? `: **${items}**` : ''}. Please register your full name so we can send you your equipment form.`,
          components: [registerRow('equip')],
        });
        return reply(ok ? `${target} is not registered yet — asked them via DM to register.` : `${target} is not registered and DMs are closed.`);
      }
      if (out.kind === 'link') {
        const ok = await dm(target, { embeds: [docEmbed(out.sub, introFor('equip', out.sub, items))], components: [linkRow(out.sub.url, 'Open equipment form')] });
        return reply(ok ? `📦 Sent the equipment form to ${target}.` : `Could not DM ${target}. Link: ${out.sub.url}`);
      }
      return reply('Unexpected state.');
    }

    case 'docs': {
      if (!staff) return message.reply('Staff only. Run this command in the server.');
      const status = first === 'status' && rest.length === 0;
      const send = first === 'send' && rest.length === 1;
      if (!status && !send) return help('Usage: !docs status @member or !docs send @member <template-slug>');
      const target = await findMember(second);
      if (!target) return help('Mention a member of this server or enter their Discord user ID.');
      if (status) {
        try {
          const v = await api.person(target.id);
          const lines = v.submissions.map((s) => `• **${s.template_name}** — ${s.valid ? `valid until ${fmt(s.valid_until)}` : s.status}`);
          return reply({ content: `**${v.person.name}** (${v.discord_accounts.map((a) => a.discord_id).join(', ')})\n${lines.join('\n') || 'no documents'}` });
        } catch (e) {
          if (e instanceof ApiError && e.status === 404) return reply({ content: `${target} is not registered.` });
          throw e;
        }
      }
      const [slug] = rest;
      const purpose = `tpl:${slug}`;
      const out = await resolve(target, purpose);
      if (out.kind === 'need_name') {
        const ok = await dm(target, { content: 'You have a document to sign. Please register your full name first.', components: [registerRow(purpose)] });
        return reply(ok ? `${target} is not registered yet — asked them via DM.` : `${target} is not registered and DMs are closed.`);
      }
      if (out.kind !== 'link') return reply('Unexpected state.');
      const ok = await dm(target, { embeds: [docEmbed(out.sub, introFor(purpose, out.sub))], components: [linkRow(out.sub.url)] });
      return reply(ok ? `Sent **${out.sub.template_name}** to ${target}.` : `Could not DM ${target}. Link: ${out.sub.url}`);
    }

    case 'link-account': {
      if (!staff) return message.reply('Staff only. Run this command in the server.');
      const oldId = first;
      if (!/^\d+$/.test(oldId) || rest.length) return help('Usage: !link-account <old-user-id> @new-member');
      const target = await findMember(second);
      if (!target) return help('Mention the replacement member or enter their Discord user ID.');
      const v = await api.linkAccounts(oldId, target.id, target.username);
      return reply({ content: `🔗 Linked ${target} to **${v.person.name}** (${v.submissions.length} documents kept).` });
    }
    default:
      return help('Unknown command.');
  }
}

client.on(Events.MessageCreate, async (message) => {
  if (message.author.bot || !message.content.startsWith('!')) return;
  if (config.guildId && message.guildId && message.guildId !== config.guildId) return;
  const input = message.content.slice(1).trim();
  const command = input.split(/\s+/, 1)[0];
  try {
    await handleCommand(message, command.toLowerCase(), input.slice(command.length).trim());
  } catch (error) {
    console.error('message command failed', error);
    const content = error instanceof ApiError ? error.message : 'Something went wrong. Please try again later.';
    await privateReply(message, { content }).catch(console.error);
  }
});

async function handleInteraction(i: Interaction) {
  if (i.isButton() && i.customId.startsWith('regbtn:')) return i.showModal(registerModal(i.customId.slice(7)));

  if (i.isModalSubmit() && i.customId.startsWith('reg:')) {
    const purpose = i.customId.slice(4);
    const name = i.fields.getTextInputValue('name').trim();
    await api.register(i.user.id, name, i.user.username, i.fields.getTextInputValue('zip').trim());
    if (purpose === 'none') return i.reply({ content: `✅ Registered as **${name}**.`, flags: MessageFlags.Ephemeral });
    const out = await resolve(i.user, purpose);
    if (out.kind === 'valid') return i.reply({ content: `✅ Registered as **${name}**. Your waiver is valid until ${fmt(out.sub.valid_until)}.`, flags: MessageFlags.Ephemeral });
    if (out.kind === 'link') {
      return i.reply({ content: `✅ Registered as **${name}**.`, embeds: [docEmbed(out.sub, introFor(purpose, out.sub))], components: [linkRow(out.sub.url)], flags: MessageFlags.Ephemeral });
    }
  }
}

client.on(Events.InteractionCreate, async (i) => {
  try {
    await handleInteraction(i);
  } catch (e) {
    console.error(e);
    const msg = e instanceof ApiError ? `⚠️ ${e.message}` : '⚠️ Something went wrong. Please try again later.';
    if (i.isRepliable()) {
      if (i.deferred || i.replied) await i.editReply({ content: msg }).catch(() => {});
      else await i.reply({ content: msg, flags: MessageFlags.Ephemeral }).catch(() => {});
    }
  }
});

// Clicking "Interested" on a Discord scheduled event counts as signing up.
client.on(Events.GuildScheduledEventUserAdd, async (event, user) => {
  if (!config.watchScheduledEvents) return;
  try {
    const name = event.name ?? 'the event';
    const out = await resolve(user, 'event');
    if (out.kind === 'valid') return;
    const ok =
      out.kind === 'need_name'
        ? await dm(user, { content: `Thanks for signing up for **${name}**! Please register your full name so we can send you the waiver to sign.`, components: [registerRow('event')] })
        : await dm(user, { content: `Thanks for signing up for **${name}**!`, embeds: [docEmbed(out.sub, introFor('event', out.sub))], components: [linkRow(out.sub.url, 'Sign waiver')] });
    if (!ok) await notifyStaff(`⚠️ ${user} signed up for **${name}** but has no valid waiver and their DMs are closed.`);
  } catch (e) {
    console.error('scheduled event handler', e);
  }
});

client.once(Events.ClientReady, async (c) => {
  console.log(`Logged in as ${c.user.tag}. Listening for ! commands.`);
  if (process.env.REGISTER_COMMANDS === 'false') return;

  // Retire the old pdfsign commands while preserving other application commands.
  const managers = [c.application.commands, ...c.guilds.cache.map((guild) => guild.commands)];
  for (const manager of managers) {
    try {
      const registered = await manager.fetch({});
      for (const command of registered.values()) {
        if (command.type === ApplicationCommandType.ChatInput && legacyCommands.has(command.name)) await command.delete();
      }
    } catch (error) {
      console.error('legacy slash command cleanup failed', error);
    }
  }
});
client.login(config.token);
