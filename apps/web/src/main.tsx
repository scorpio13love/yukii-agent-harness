import { StrictMode } from 'react';
import { createRoot } from 'react-dom/client';
import { Workbench } from '@yukii/workbench-ui';
import '@yukii/workbench-ui/styles.css';

createRoot(document.getElementById('root')!).render(
  <StrictMode>
    <Workbench />
  </StrictMode>,
);
