import { PermissionFlagsBits, SlashCommandBuilder } from 'discord.js';

export const commands = [
  new SlashCommandBuilder()
    .setName('register')
    .setDescription('Register or update your full name (needed to open your documents)')
    .addStringOption((o) => o.setName('name').setDescription('Your full legal name').setRequired(true).setMinLength(3).setMaxLength(120))
    .addStringOption((o) => o.setName('zip').setDescription('Your zip code (tells people with the same name apart)').setRequired(true).setMinLength(3).setMaxLength(10)),

  new SlashCommandBuilder()
    .setName('signup')
    .setDescription('Sign up for an event (checks your waiver and sends a signing link if needed)')
    .addStringOption((o) => o.setName('event').setDescription('Event name').setRequired(false)),

  new SlashCommandBuilder().setName('documents').setDescription('Show your documents and their links'),

  new SlashCommandBuilder().setName('equipment').setDescription('Open your equipment form (add items you received)'),

  new SlashCommandBuilder()
    .setName('handout')
    .setDescription('Staff: hand out equipment — sends the recipient their equipment form')
    .setDefaultMemberPermissions(PermissionFlagsBits.ManageGuild)
    .setDMPermission(false)
    .addUserOption((o) => o.setName('user').setDescription('Recipient').setRequired(true))
    .addStringOption((o) => o.setName('items').setDescription('What was handed out (shown in the DM)').setRequired(false)),

  new SlashCommandBuilder()
    .setName('docs')
    .setDescription('Staff: manage member documents')
    .setDefaultMemberPermissions(PermissionFlagsBits.ManageGuild)
    .setDMPermission(false)
    .addSubcommand((s) =>
      s.setName('status').setDescription('Show the documents of a member').addUserOption((o) => o.setName('user').setDescription('Member').setRequired(true)),
    )
    .addSubcommand((s) =>
      s
        .setName('send')
        .setDescription('Send a signing link for any template')
        .addUserOption((o) => o.setName('user').setDescription('Member').setRequired(true))
        .addStringOption((o) => o.setName('template').setDescription('Template').setRequired(true).setAutocomplete(true)),
    ),

  new SlashCommandBuilder()
    .setName('link-account')
    .setDescription('Staff: link a new Discord account to the data of a lost one')
    .setDefaultMemberPermissions(PermissionFlagsBits.ManageGuild)
    .setDMPermission(false)
    .addStringOption((o) => o.setName('old_id').setDescription('Discord user id of the lost account').setRequired(true))
    .addUserOption((o) => o.setName('new_user').setDescription('The new account').setRequired(true)),
];
