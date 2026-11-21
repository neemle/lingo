import {defineConfig} from '@playwright/test';
export default defineConfig({testDir:'tests/ui',timeout:30000,use:{baseURL:'http://127.0.0.1:1420',launchOptions:{executablePath:'/Applications/Google Chrome.app/Contents/MacOS/Google Chrome'},viewport:{width:1440,height:1000}},webServer:{command:'npm start',url:'http://127.0.0.1:1420',reuseExistingServer:true},reporter:'list'});
