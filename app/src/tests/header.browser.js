// Run in a 600x800 browser against the built app. Fixtures are DOM-only and removed.
(async () => {
 const check=(condition,message)=>{if(!condition)throw Error(message)};
 const header=document.querySelector('.topbar'),list=document.querySelector('#rule-list');
 const fixture=document.createElement('div');fixture.style.height='3000px';list.append(fixture);
 const initial=header.getBoundingClientRect().y;
 window.scrollTo(0,1000);await new Promise(r=>setTimeout(r,50));
 const scrolled=header.getBoundingClientRect().y,style=getComputedStyle(header);
 check(scrollY>=1000,'fixture must scroll the document');
 check(scrolled===initial,'header moved during scroll');
 check(style.backgroundColor!=='rgba(0, 0, 0, 0)','header must be opaque');
 const rect=header.getBoundingClientRect();
 check(header.contains(document.elementFromPoint(rect.x+rect.width/2,rect.y+rect.height/2)),'content overlaps pinned header');
 document.querySelector('[data-page=settings]').click();
 check(!document.querySelector('[data-page-panel=settings]').hidden,'settings tab is not clickable');
 document.querySelector('[data-page=intercept]').click();
 document.querySelector('#add-button').click();
 check(!document.querySelector('#rule-modal').hidden,'add button is not clickable');
 const modal=document.querySelector('.modal').getBoundingClientRect();
 check(document.querySelector('#rule-modal').contains(document.elementFromPoint(modal.x+20,modal.y+20)),'header obscures modal');
 document.querySelector('#cancel-modal').click();
 const toggle=document.querySelector('#global-toggle'),before=toggle.checked;
 document.querySelector('.top-actions .toggle-track').click();
 await new Promise(r=>setTimeout(r,30));
 check(toggle.checked!==before,'switch is not clickable');
 fixture.remove();window.scrollTo(0,0);
 return {viewport:[innerWidth,innerHeight],headerYBefore:initial,headerYAfter:scrolled,background:style.backgroundColor,controls:'tabs, add, modal, switch passed'};
})()
