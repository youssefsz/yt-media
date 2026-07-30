import { mount } from 'svelte';

import App from './App.svelte';
import './app.css';

const target = document.querySelector<HTMLElement>('#app');

if (target === null) {
  throw new Error('application mount point was not found');
}

target.replaceChildren();
mount(App, { target });
