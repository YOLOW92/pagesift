// Native local RequestQueue + Dataset. No extra deduplication or recovery layer.
import { createRequire } from 'node:module';
import { resolve } from 'node:path';
import { appendFileSync, writeFileSync } from 'node:fs';
const require=createRequire(resolve('bench/data/six/node/package.json'));
const { CheerioCrawler, RequestQueue, Dataset, log }=require('@crawlee/cheerio');
log.setLevel(log.LEVELS.ERROR);
const [url, events, output]=process.argv.slice(2);
const queue=await RequestQueue.open('recovery');
await queue.addRequest({url});
const crawler=new CheerioCrawler({requestQueue:queue,maxConcurrency:2,minConcurrency:1,
    useSessionPool:false,respectRobotsTxtFile:true,maxRequestRetries:1,
    async requestHandler({request,enqueueLinks}) {
        await enqueueLinks({strategy:'same-origin'});
        await Dataset.pushData({url:request.url});
        appendFileSync(events,JSON.stringify({url:request.url})+'\n');
    }});
await crawler.run();
const data=await Dataset.getData({limit:1000});
writeFileSync(output,JSON.stringify(data));
