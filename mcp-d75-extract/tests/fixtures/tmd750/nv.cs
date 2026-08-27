public class nv
{
	private List<m5> m_o;

	public void ai()
	{
		int num3 = 0;
		while (num3 < 6)
		{
			this.m_o.Add(new m5());
			this.m_o[num3].OffsetProgrammableMemoryAddress = 8192 * num3;
			num3++;
		}
	}

	public void a6(n7 A_0)
	{
		int num3 = 0;
		while (num3 < 6)
		{
			this.m_o[num3].a6(A_0);
			num3++;
		}
	}

	public void a7(n7 A_0)
	{
		this.m_o[0].a7(A_0);
	}
}
