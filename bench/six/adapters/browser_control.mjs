// Browser capability control, distinct from adaptive prediction. Network-idle wait
// is explicit and charged to this mode; these observations are not speed claims.
import { createRequire } from 'node:module';
import { readFileSync, writeFileSync } from 'node:fs';
import { resolve } from 'node:path';
const require=createRequire(resolve('bench/data/six/node/package.json'));
const { PlaywrightCrawler, log }=require('@crawlee/playwright');
log.setLevel(log.LEVELS.ERROR);
const [manifest,output]=process.argv.slice(2);
const requests=JSON.parse(readFileSync(manifest,'utf8'));
const items=[];
const crawler=new PlaywrightCrawler({maxConcurrency:1,maxRequestsPerCrawl:requests.length,
  maxRequestRetries:0,useSessionPool:false,respectRobotsTxtFile:true,
  launchContext:{launchOptions:{executablePath:'/opt/google/chrome/google-chrome',headless:true}},
  async requestHandler({request,page}) {
    await page.waitForLoadState('networkidle');
    items.push({url:request.url,text:await page.locator('article').innerText()});
  }});
await crawler.run(requests);
writeFileSync(output,JSON.stringify({items,mode:'native PlaywrightCrawler, explicit networkidle wait'},null,2));
