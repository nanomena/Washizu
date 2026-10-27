import { StrictMode } from 'react'
import { createRoot } from 'react-dom/client'
import './index.css'
import { Monitor } from '@/routes/Monitor'
import { Profiles } from '@/routes/Profiles'

// The interface is one page: the monitor's dock, with the stage the shell puts
// the game on above it.
const profilesView = new URLSearchParams(location.search).get('view') === 'profiles'
createRoot(document.getElementById('root')!).render(<StrictMode>{profilesView ? <Profiles /> : <Monitor />}</StrictMode>)
