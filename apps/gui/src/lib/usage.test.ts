import assert from "node:assert/strict";
import { test } from "node:test";
import { compactUsage, comparison, smoothPath } from "./usage.ts";
test("compact values and compare equal windows",()=>{assert.equal(compactUsage(1500000),"1.5M"); assert.equal(comparison(144,100),"↑ 44%"); assert.equal(comparison(1,0),null); assert.equal(comparison(1,null),null);});
test("monotone interpolation flattens at a local extremum",()=>{assert.equal(smoothPath([{x:0,y:0},{x:3,y:3},{x:6,y:0}]),"M0,0 C1,1 2,3 3,3 C4,3 5,1 6,0");});
