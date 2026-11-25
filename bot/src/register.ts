// Registers the slash commands. Run once after changes: `bun run register`.
import { REST, Routes } from 'discord.js';
import { commands } from './commands';
import { config } from './config';

const rest = new REST().setToken(config.token);
const body = commands.map((c) => c.toJSON());
const route = config.guildId ? Routes.applicationGuildCommands(config.clientId, config.guildId) : Routes.applicationCommands(config.clientId);
await rest.put(route, { body });
console.log(`Registered ${body.length} commands ${config.guildId ? `for guild ${config.guildId}` : 'globally'}`);
