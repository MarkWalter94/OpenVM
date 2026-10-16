/* Bounded CF_DIB/V5 clipboard images, streamed as LIMG + dimensions + RGBA8.
 * Included after the JSON, allocation, crypto and hex helpers in tools.c. */
#define IMAGE_PIXELS (4u*1024u*1024u)
#define IMAGE_MAX (12u+IMAGE_PIXELS*4u)
typedef struct {BYTE *data;DWORD size,written;char id[65],digest[65];HCRYPTHASH hash;} ClipboardImage;
static ClipboardImage image_capture,image_upload;
static DWORD image_u32(const BYTE *p){return (DWORD)p[0]|((DWORD)p[1]<<8)|((DWORD)p[2]<<16)|((DWORD)p[3]<<24);}
static void image_put32(BYTE *p,DWORD n){p[0]=(BYTE)n;p[1]=(BYTE)(n>>8);p[2]=(BYTE)(n>>16);p[3]=(BYTE)(n>>24);}
static void image_clear(ClipboardImage *image){if(image->hash)CryptDestroyHash(image->hash);release(image->data,image->size);SecureZeroMemory(image,sizeof(*image));}
static int image_geometry(const BYTE *data,DWORD size,DWORD *width,DWORD *height){
    if(size<12||data[0]!='L'||data[1]!='I'||data[2]!='M'||data[3]!='G')return 0;*width=image_u32(data+4);*height=image_u32(data+8);
    return *width&&*height&&*width<=8192&&*height<=8192&&(uint64_t)(*width)*(*height)<=IMAGE_PIXELS&&size==12+(*width)*(*height)*4;
}
static int image_capture_native(ClipboardImage *image){
    HANDLE handle;BYTE *dib;SIZE_T bytes;BITMAPINFOHEADER *h;DWORD width,height,stride,x,y,step;int ok=0,alpha=0;
    if(!clipboard_open())return 0;
    handle=GetClipboardData(IsClipboardFormatAvailable(CF_DIBV5)?CF_DIBV5:CF_DIB);
    bytes=handle?GlobalSize(handle):0;dib=handle?GlobalLock(handle):NULL;
    if(!dib||bytes<sizeof(BITMAPINFOHEADER))goto done;
    h=(BITMAPINFOHEADER *)dib;
    if((h->biSize!=40&&h->biSize!=108&&h->biSize!=124)||h->biSize>bytes||h->biWidth<=0||h->biWidth>8192||h->biHeight==0||h->biHeight>8192||h->biHeight< -8192||h->biPlanes!=1||(h->biBitCount!=24&&h->biBitCount!=32))goto done;
    width=(DWORD)h->biWidth;height=(DWORD)(h->biHeight<0?-h->biHeight:h->biHeight);
    if((uint64_t)width*height>IMAGE_PIXELS)goto done;
    if(h->biCompression==BI_BITFIELDS){
        BITMAPV5HEADER *v=(BITMAPV5HEADER *)dib;
        if(h->biSize!=124||h->biBitCount!=32||v->bV5RedMask!=0x00ff0000||v->bV5GreenMask!=0x0000ff00||v->bV5BlueMask!=0x000000ff||(v->bV5AlphaMask&&v->bV5AlphaMask!=0xff000000))goto done;
        alpha=v->bV5AlphaMask!=0;
    }else if(h->biCompression!=BI_RGB)goto done;
    else if(h->biSize==124&&((BITMAPV5HEADER *)dib)->bV5AlphaMask==0xff000000)alpha=1;
    stride=((width*h->biBitCount+31)/32)*4;step=h->biBitCount/8;
    if((uint64_t)h->biSize+(uint64_t)stride*height>bytes)goto done;
    image->size=12+width*height*4;image->data=alloc(image->size);if(!image->data)goto done;
    memcpy(image->data,"LIMG",4);image_put32(image->data+4,width);image_put32(image->data+8,height);
    for(y=0;y<height;++y){const BYTE *src=dib+h->biSize+(h->biHeight>0?height-1-y:y)*stride;BYTE *dst=image->data+12+y*width*4;
        for(x=0;x<width;++x){dst[4*x]=src[step*x+2];dst[4*x+1]=src[step*x+1];dst[4*x+2]=src[step*x];dst[4*x+3]=alpha?src[step*x+3]:255;}}
    ok=1;
done:if(dib)GlobalUnlock(handle);CloseClipboard();if(!ok)image_clear(image);return ok;
}
static int image_publish(const ClipboardImage *image){
    DWORD width,height,x,y;HGLOBAL memory;BYTE *dib;BITMAPV5HEADER *h;int ok=0;
    if(!image_geometry(image->data,image->size,&width,&height))return 0;
    memory=GlobalAlloc(GMEM_MOVEABLE,sizeof(BITMAPV5HEADER)+width*height*4);if(!memory)return 0;
    dib=GlobalLock(memory);if(!dib){GlobalFree(memory);return 0;}
    h=(BITMAPV5HEADER *)dib;memset(h,0,sizeof(*h));h->bV5Size=sizeof(*h);h->bV5Width=(LONG)width;h->bV5Height=(LONG)height;h->bV5Planes=1;h->bV5BitCount=32;h->bV5Compression=BI_BITFIELDS;h->bV5SizeImage=width*height*4;
    h->bV5RedMask=0x00ff0000;h->bV5GreenMask=0x0000ff00;h->bV5BlueMask=0x000000ff;h->bV5AlphaMask=0xff000000;h->bV5CSType=0x73524742u; /* LCS_sRGB */
    for(y=0;y<height;++y){const BYTE *src=image->data+12+y*width*4;BYTE *dst=dib+sizeof(*h)+(height-1-y)*width*4;
        for(x=0;x<width;++x){dst[4*x]=src[4*x+2];dst[4*x+1]=src[4*x+1];dst[4*x+2]=src[4*x];dst[4*x+3]=src[4*x+3];}}
    GlobalUnlock(memory);
    if(clipboard_open()){if(EmptyClipboard()&&SetClipboardData(CF_DIBV5,memory)){ok=1;memory=NULL;}CloseClipboard();}
    if(memory)GlobalFree(memory);return ok;
}
static int image_path(JDoc *doc,int request){int p=jget(doc,request,"path");size_t i;unsigned count=0;if(p<0||doc->tokens[p].kind!=J_ARRAY)return 0;for(i=(size_t)p+1;i<doc->count&&doc->tokens[i].start<doc->tokens[p].end;++i)if(doc->tokens[i].parent==p){if(!jequal(doc,(int)i,"image"))return 0;++count;}return count==1;}
static void clipboard_image(JOut *out,JDoc *doc,int request){
    int op=jget(doc,request,"op");char token[65],*hex=NULL;BYTE *bytes=NULL;size_t count=0;ClipboardImage *image=NULL;
    if(jequal(doc,op,"begin")){
        BYTE nonce[32];uint64_t size=jnumber(doc,jget(doc,request,"size"),UINT64_MAX);
        if(!image_path(doc,request)||token_kind(doc,jget(doc,request,"replace"))==J_TRUE||!crypto||!CryptGenRandom(crypto,32,nonce))goto bad;
        if(jequal(doc,jget(doc,request,"share"),"clipboard-read")){
            if(image_capture.data||size||!image_capture_native(&image_capture))goto bad;image=&image_capture;
        }else if(jequal(doc,jget(doc,request,"share"),"clipboard-write")){
            if(image_upload.data||size<12||size>IMAGE_MAX)goto bad;image=&image_upload;
            image->size=(DWORD)size;image->data=alloc(image->size);
            if(!image->data||!jstring(doc,jget(doc,request,"sha256"),image->digest,65)||strlen(image->digest)!=64||!CryptCreateHash(crypto,CALG_SHA_256,0,0,&image->hash)){image_clear(image);goto bad;}
        }else goto bad;
        hex_encode(nonce,32,image->id);jtext(out,"{\"result\":\"transfer\",\"transfer\":");jquote(out,image->id);jtext(out,"}");return;
    }
    if(jequal(doc,op,"read")){
        uint64_t offset=jnumber(doc,jget(doc,request,"offset"),UINT64_MAX),length=jnumber(doc,jget(doc,request,"length"),0);
        if(!image_capture.data||!image_path(doc,request)||!jstring(doc,jget(doc,request,"share"),token,65)||!equal(token,image_capture.id)||offset>=image_capture.size||!length||length>16384)goto bad;
        count=(size_t)(image_capture.size-offset);if(count>length)count=(size_t)length;hex=alloc(count*2+1);if(!hex)goto bad;
        hex_encode(image_capture.data+(size_t)offset,count,hex);jtext(out,"{\"result\":\"data\",\"offset\":");juint(out,offset);jtext(out,",\"data\":");jquote(out,hex);jtext(out,",\"eof\":");jtext(out,offset+count==image_capture.size?"true}":"false}");release(hex,count*2+1);return;
    }
    if(!jstring(doc,jget(doc,request,"transfer"),token,65))goto bad;
    if(jequal(doc,op,"cancel")){if(equal(token,image_capture.id))image_clear(&image_capture);if(equal(token,image_upload.id))image_clear(&image_upload);jtext(out,"{\"result\":\"ok\"}");return;}
    image=&image_upload;if(!image->data||!equal(token,image->id))goto bad;
    if(jequal(doc,op,"write")){
        hex=alloc(32769);bytes=alloc(16384);if(!hex||!bytes)goto upload_bad;
        if(!jstring(doc,jget(doc,request,"data"),hex,32769)||!hex_decode(hex,bytes,16384,&count)||!count||jnumber(doc,jget(doc,request,"offset"),UINT64_MAX)!=image->written||count>image->size-image->written||!CryptHashData(image->hash,bytes,(DWORD)count,0))goto upload_bad;
        memcpy(image->data+image->written,bytes,count);image->written+=(DWORD)count;release(hex,32769);release(bytes,16384);
    }else if(jequal(doc,op,"commit")){
        BYTE digest[32];DWORD n=32;char actual[65];int ok;
        if(image->written!=image->size||!CryptGetHashParam(image->hash,HP_HASHVAL,digest,&n,0))goto upload_bad;
        hex_encode(digest,32,actual);ok=equal(actual,image->digest)&&image_publish(image);image_clear(image);if(!ok)goto bad;
    }else goto bad;
    jtext(out,"{\"result\":\"ok\"}");return;
upload_bad:release(hex,32769);release(bytes,16384);image_clear(image);
bad:error_reply(out,"image_clipboard_refused");
}
