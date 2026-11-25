import {
  ActionRowBuilder,
  ButtonBuilder,
  ButtonStyle,
  ChatInputCommandInteraction,
  Client,
  EmbedBuilder,
  Events,
  GatewayIntentBits,
  GuildMember,
  MessageFlags,
  ModalBuilder,
  Partials,
  TextInputBuilder,
  TextInputStyle,
  type Interaction,
  type User,
} from 'discord.js';
import { REST, Routes } from 'discord.js';
import { api, ApiError, type Submission } from './api';
import { commands } from './commands';
import { config } from './config';

const client = new Client({
  intents: [GatewayIntentBits.Guilds, GatewayIntentBits.GuildScheduledEvents, GatewayIntentBits.DirectMessages],
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

async function isStaff(i: ChatInputCommandInteraction) {
  const m = i.member;
  if (!(m instanceof GuildMember)) return false;
  if (config.adminRoleIds.length) return m.roles.cache.some((r) => config.adminRoleIds.includes(r.id));
  return m.permissions.has('ManageGuild');
}

async function handleCommand(i: ChatInputCommandInteraction) {
  switch (i.commandName) {
    case 'register': {
      const name = i.options.getString('name', true);
      await api.register(i.user.id, name, i.user.username, i.options.getString('zip', true));
      return i.reply({ content: `✅ Registered as **${name}**. Use this name to open your documents.`, flags: MessageFlags.Ephemeral });
    }

    case 'signup':
    case 'equipment': {
      const purpose = i.commandName === 'signup' ? 'event' : 'equip';
      const out = await resolve(i.user, purpose);
      if (out.kind === 'need_name') return i.showModal(registerModal(purpose));
      if (out.kind === 'valid')
        return i.reply({ content: `✅ You're all set${i.options.getString('event') ? ` for **${i.options.getString('event')}**` : ''} — your waiver is valid until ${fmt(out.sub.valid_until)}.`, flags: MessageFlags.Ephemeral });
      return i.reply({ embeds: [docEmbed(out.sub, introFor(purpose, out.sub))], components: [linkRow(out.sub.url)], flags: MessageFlags.Ephemeral });
    }

    case 'documents': {
      let view;
      try {
        view = await api.person(i.user.id);
      } catch (e) {
        if (e instanceof ApiError && e.status === 404) return i.showModal(registerModal('none'));
        throw e;
      }
      const subs = view.submissions.filter((s) => s.status !== 'revoked');
      if (!subs.length) return i.reply({ content: 'You have no documents yet.', flags: MessageFlags.Ephemeral });
      const lines = subs.map((s) => `• **${s.template_name}** — ${s.valid ? `✅ valid until ${fmt(s.valid_until)}` : s.status === 'pending' ? '✍️ waiting for your signature' : `⚠️ ${s.status}`}`);
      const rows = subs.slice(0, 5).map((s) => linkRow(s.url, s.template_name.slice(0, 70)));
      return i.reply({ content: `Registered as **${view.person.name}**\n${lines.join('\n')}`, components: rows, flags: MessageFlags.Ephemeral });
    }

    case 'handout': {
      if (!(await isStaff(i))) return i.reply({ content: 'Staff only.', flags: MessageFlags.Ephemeral });
      const target = i.options.getUser('user', true);
      const items = i.options.getString('items') ?? undefined;
      await i.deferReply({ flags: MessageFlags.Ephemeral });
      const out = await resolve(target, 'equip');
      if (out.kind === 'need_name') {
        const ok = await dm(target, {
          content: `You received equipment${items ? `: **${items}**` : ''}. Please register your full name so we can send you your equipment form.`,
          components: [registerRow('equip')],
        });
        return i.editReply(ok ? `${target} is not registered yet — asked them via DM to register.` : `${target} is not registered and DMs are closed.`);
      }
      if (out.kind === 'link') {
        const ok = await dm(target, { embeds: [docEmbed(out.sub, introFor('equip', out.sub, items))], components: [linkRow(out.sub.url, 'Open equipment form')] });
        return i.editReply(ok ? `📦 Sent the equipment form to ${target}.` : `Could not DM ${target}. Link: ${out.sub.url}`);
      }
      return i.editReply('Unexpected state.');
    }

    case 'docs': {
      if (!(await isStaff(i))) return i.reply({ content: 'Staff only.', flags: MessageFlags.Ephemeral });
      const target = i.options.getUser('user', true);
      if (i.options.getSubcommand() === 'status') {
        try {
          const v = await api.person(target.id);
          const lines = v.submissions.map((s) => `• **${s.template_name}** — ${s.valid ? `valid until ${fmt(s.valid_until)}` : s.status}`);
          return i.reply({ content: `**${v.person.name}** (${v.discord_accounts.map((a) => a.discord_id).join(', ')})\n${lines.join('\n') || 'no documents'}`, flags: MessageFlags.Ephemeral });
        } catch (e) {
          if (e instanceof ApiError && e.status === 404) return i.reply({ content: `${target} is not registered.`, flags: MessageFlags.Ephemeral });
          throw e;
        }
      }
      const slug = i.options.getString('template', true);
      await i.deferReply({ flags: MessageFlags.Ephemeral });
      const purpose = `tpl:${slug}`;
      const out = await resolve(target, purpose);
      if (out.kind === 'need_name') {
        const ok = await dm(target, { content: 'You have a document to sign. Please register your full name first.', components: [registerRow(purpose)] });
        return i.editReply(ok ? `${target} is not registered yet — asked them via DM.` : `${target} is not registered and DMs are closed.`);
      }
      if (out.kind !== 'link') return i.editReply('Unexpected state.');
      const ok = await dm(target, { embeds: [docEmbed(out.sub, introFor(purpose, out.sub))], components: [linkRow(out.sub.url)] });
      return i.editReply(ok ? `Sent **${out.sub.template_name}** to ${target}.` : `Could not DM ${target}. Link: ${out.sub.url}`);
    }

    case 'link-account': {
      if (!(await isStaff(i))) return i.reply({ content: 'Staff only.', flags: MessageFlags.Ephemeral });
      const oldId = i.options.getString('old_id', true).trim();
      const target = i.options.getUser('new_user', true);
      const v = await api.linkAccounts(oldId, target.id, target.username);
      return i.reply({ content: `🔗 Linked ${target} to **${v.person.name}** (${v.submissions.length} documents kept).`, flags: MessageFlags.Ephemeral });
    }
  }
}

async function handleInteraction(i: Interaction) {
  if (i.isChatInputCommand()) return handleCommand(i);

  if (i.isAutocomplete() && i.commandName === 'docs') {
    const q = i.options.getFocused().toLowerCase();
    const list = await api.templates();
    return i.respond(list.filter((t) => t.name.toLowerCase().includes(q) || t.slug.includes(q)).slice(0, 25).map((t) => ({ name: t.name.slice(0, 100), value: t.slug })));
  }

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
  console.log(`Logged in as ${c.user.tag}`);
  if (process.env.REGISTER_COMMANDS !== 'false') {
    const rest = new REST().setToken(config.token);
    const route = config.guildId ? Routes.applicationGuildCommands(config.clientId, config.guildId) : Routes.applicationCommands(config.clientId);
    await rest.put(route, { body: commands.map((cmd) => cmd.toJSON()) }).catch((e) => console.error('command registration failed', e));
    console.log('Slash commands registered');
  }
});
client.login(config.token);
