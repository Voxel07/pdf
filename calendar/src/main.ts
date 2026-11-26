import { mount } from 'svelte';
import App from './App.svelte';
import './app.css';

const target = document.getElementById('app');
if (!target) throw new Error('Calendar mount point missing.');
mount(App, { target });
