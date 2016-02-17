pub use self::entry::*;
pub use self::mapper::Mapper;

use memory::{PAGE_SIZE, Frame, FrameAllocator}; // needed later
use self::table::{Table, Level4};
use core::ptr::Unique;
use self::temporary_page::TemporaryPage;
use core::ops::{Deref, DerefMut};
use multiboot2::BootInformation;


mod entry;
mod table;
mod temporary_page;
mod mapper;

const ENTRY_COUNT: usize = 512;

pub type PhysicalAddress = usize;
pub type VirtualAddress = usize;


#[derive(Debug, Clone, Copy)]
pub struct Page {
    number: usize,
}

impl Page {
    fn start_address(&self) -> usize {
        self.number * PAGE_SIZE
    }

    pub fn containing_address(address: VirtualAddress) -> Page {
        assert!(address < 0x0000_8000_0000_0000 || address >= 0xffff_8000_0000_0000,
            "invalid address: 0x{:x}", address);
        Page { number: address / PAGE_SIZE }
    }

    fn p4_index(&self) -> usize {
        (self.number >> 27) & 0o777
    }

    fn p3_index(&self) -> usize {
        (self.number >> 18) & 0o777
    }

    fn p2_index(&self) -> usize {
        (self.number >> 9) & 0o777
    }

    fn p1_index(&self) -> usize {
        (self.number >> 0) & 0o777
    }

}


pub struct ActivePageTable {
    mapper: Mapper,
}

impl Deref for ActivePageTable {
    type Target = Mapper;

    fn deref(&self) -> &Mapper {
        &self.mapper
    }
}

impl DerefMut for ActivePageTable {
    fn deref_mut(&mut self) -> &mut Mapper {
        &mut self.mapper
    }
}

impl ActivePageTable {
    unsafe fn new() -> ActivePageTable {
        ActivePageTable {
            mapper: Mapper::new(),
        }
    }

    pub fn with<F>(&mut self,
               table: &mut InactivePageTable,
               temporary_page: &mut temporary_page::TemporaryPage,
               f: F)
        where F: FnOnce(&mut Mapper)
    {
        use x86::{controlregs, tlb};
        let flush_tlb = || unsafe { tlb::flush_all() };

        {
            let backup = Frame::containing_address(
                unsafe { controlregs::cr3() } as usize);

            // map temporary_page to current p4 table
            let p4_table = temporary_page.map_table_frame(backup.clone(), self);

            // overwrite recursive mapping
            self.p4_mut()[511].set(table.p4_frame.clone(), PRESENT | WRITABLE);
            flush_tlb();

            // execute f in the new context
            f(self);

            // Restory recursive mapping to its original p4 table.
            p4_table[511].set(backup, PRESENT | WRITABLE);
            flush_tlb();
        }

        temporary_page.unmap(self);
    }
}


pub struct InactivePageTable {
    p4_frame: Frame,
}

impl InactivePageTable {
    pub fn new(frame: Frame,
               active_table: &mut ActivePageTable,
               temporary_page: &mut TemporaryPage)
               -> InactivePageTable {
        {
            let table = temporary_page.map_table_frame(frame.clone(), active_table);
            // now we are able to zero the table
            table.zero();
            // set up recursive mapping for the table
            table[511].set(frame.clone(), PRESENT | WRITABLE);
        }
        temporary_page.unmap(active_table);

        InactivePageTable { p4_frame: frame }
    }
}


pub fn remap_the_kernel<A>(allocator: &mut A, boot_info: &BootInformation)
    where A: FrameAllocator
{
    use core::ops::Range;

    let mut temporary_page = TemporaryPage::new(Page { number: 0xcafebabe },
        allocator);

    let mut active_table = unsafe { ActivePageTable::new() };
    let mut new_table = {
        let frame = allocator.allocate_frame().expect("no more frames");
        InactivePageTable::new(frame, &mut active_table, &mut temporary_page)
    };

    active_table.with(&mut new_table, &mut temporary_page, |mapper| {
        let elf_sections_tag = boot_info.elf_sections_tag()
            .expect("Memory map tag required");

        for section in elf_sections_tag.sections() {
            use multiboot2::ELF_SECTION_ALLOCATED;
            use self::entry::WRITABLE;

            if !section.flags().contains(ELF_SECTION_ALLOCATED) {
                // section is not loaded to memory
                continue;
            }

            println!("mapping section at addr: {:#x}, size: {:#x}",
                section.addr, section.size);

            let flags = WRITABLE; // TODO use real section flags

            let range = Range {
                start: section.addr as usize,
                end: (section.addr + section.size) as usize,
            };
            
            for address in range.step_by(PAGE_SIZE) {
                assert!(address % PAGE_SIZE == 0,
                "sections need to be page aligned");
                let frame = Frame::containing_address(address);
                mapper.identity_map(frame, flags, allocator);
            }
        }    
    });
}
