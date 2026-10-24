import '@fontsource-variable/bricolage-grotesque';
import '@fontsource-variable/geist';
import '@fontsource-variable/geist-mono';
import './app.css';
import { mount } from 'svelte';
import App from './App.svelte';
import Identify from './pages/Identify.svelte';

const target = document.getElementById('app')!;
const identify = location.hash.match(/^#identify\/(\d+)\/(.*)$/);

export default identify
  ? mount(Identify, { target, props: { n: Number(identify[1]), name: decodeURIComponent(identify[2]) } })
  : mount(App, { target });
